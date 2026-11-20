//! The administrator's side of the monitoring: the dashboard's data, the history for its
//! charts, and the few actions that go through the host agent. Each action is checked
//! against a fixed list here, again by the agent, and leaves an audit entry.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use teamtak_agent::report::RESTARTABLE_SERVICES;
use teamtak_core::policy::Action;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::audit::{self, Actor, AuditAction, Detail, Details, Target};
use crate::auth::AdminAccount;
use crate::error::{ApiError, ApiResult};
use crate::mail::scrub_error;
use crate::state::SharedState;

use super::agent::AgentCallError;
use super::alerting;
use super::history;
use super::maintenance::{self, BackupError};
use super::release;

#[derive(Serialize, sqlx::FromRow)]
struct AlertRow {
    id: i64,
    kind: String,
    subject: String,
    severity: String,
    details: Value,
    #[serde(with = "time::serde::rfc3339")]
    raised_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    last_seen_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    resolved_at: Option<OffsetDateTime>,
}

#[derive(Deserialize, Default)]
pub struct MonitoringQuery {
    /// Look now instead of serving the last look.
    #[serde(default)]
    refresh: bool,
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// `GET /api/admin/monitoring`: the latest look, the open alerts and those resolved lately.
/// `?refresh=true` looks first, which takes as long as the slowest probe.
pub async fn monitoring(State(state): State<SharedState>, AdminAccount(admin): AdminAccount, Query(query): Query<MonitoringQuery>) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::ViewMonitoring)?;
    let snapshot = if query.refresh { state.monitor.refresh_now().await } else { state.monitor.latest() };
    let open: Vec<AlertRow> = sqlx::query_as(
        "SELECT id, kind, subject, severity, details, raised_at, last_seen_at, resolved_at FROM alerts
          WHERE resolved_at IS NULL
          ORDER BY CASE severity WHEN 'critical' THEN 0 WHEN 'warning' THEN 1 ELSE 2 END, raised_at DESC",
    )
    .fetch_all(&state.db)
    .await?;
    let resolved: Vec<AlertRow> = sqlx::query_as(
        "SELECT id, kind, subject, severity, details, raised_at, last_seen_at, resolved_at FROM alerts
          WHERE resolved_at > now() - interval '7 days' ORDER BY resolved_at DESC LIMIT 50",
    )
    .fetch_all(&state.db)
    .await?;
    let settings = crate::settings::load(&state.db).await?;
    let webhook_last = state.monitor.webhook_last();
    Ok(Json(json!({
        // Null until the first look after a start is done, a few seconds.
        "snapshot": snapshot.as_deref(),
        "alerts": { "open": open, "resolved": resolved },
        "notifications": {
            "emails": settings.values.alert_emails,
            "webhook_set": !settings.values.webhook_url.is_empty(),
            "webhook_last": webhook_last.map(|(at, outcome)| json!({ "at": rfc3339(at), "ok": outcome.is_ok(), "error": outcome.err() })),
            "reminder_hours": settings.values.reminder_hours,
        },
    })))
}

/// `GET /api/admin/alerts/summary`: how many alerts are open, for the navigation's badge.
pub async fn alerts_summary(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::ViewMonitoring)?;
    let (critical, warning, low): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE severity = 'critical'), count(*) FILTER (WHERE severity = 'warning'),
                count(*) FILTER (WHERE severity = 'low')
           FROM alerts WHERE resolved_at IS NULL",
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "open": critical + warning + low, "critical": critical, "warning": warning, "low": low })))
}

#[derive(Deserialize, Default)]
pub struct HistoryQuery {
    /// The last this many hours, up to now.
    hours: Option<i64>,
    /// Or a period, in seconds since 1970: both or neither.
    from: Option<i64>,
    to: Option<i64>,
}

/// The period a history request asks for, within what is kept and never past `now`.
fn history_period(query: &HistoryQuery, now: OffsetDateTime) -> ApiResult<(OffsetDateTime, OffsetDateTime)> {
    let oldest = history::oldest(now);
    let (from, to) = match (query.from, query.to) {
        (Some(from), Some(to)) => {
            let at = |s: i64| OffsetDateTime::from_unix_timestamp(s).map_err(|_| ApiError::InvalidInput);
            (at(from)?.max(oldest), at(to)?.min(now))
        }
        (None, None) => {
            let hours = query.hours.unwrap_or(24).clamp(1, maintenance::HOUR_SAMPLES_DAYS as i64 * 24);
            (now - time::Duration::hours(hours), now)
        }
        _ => return Err(ApiError::InvalidInput),
    };
    // A minute at least: less than one sample is no chart.
    if to - from < time::Duration::minutes(1) {
        return Err(ApiError::InvalidInput);
    }
    Ok((from, to))
}

/// `GET /api/admin/monitoring/history?hours=24`, or `?from=…&to=…` in seconds since 1970: the
/// machine's points and each service's, oldest first, each the average of the samples it
/// stands for with their highest beside it (`…_max`).
pub async fn history(State(state): State<SharedState>, AdminAccount(admin): AdminAccount, Query(query): Query<HistoryQuery>) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::ViewMonitoring)?;
    let now = OffsetDateTime::now_utc();
    let (from, to) = history_period(&query, now)?;
    let plan = history::plan(from, to, now);
    let samples = history::machine(&state.db, from, to, plan).await?;
    let services = history::services(&state.db, from, to, plan).await?;
    let rfc3339 = |at: OffsetDateTime| at.format(&Rfc3339).unwrap_or_default();
    Ok(Json(json!({
        "from": rfc3339(from),
        "to": rfc3339(to),
        "oldest": rfc3339(history::oldest(now)),
        "retention_days": maintenance::METRIC_SAMPLES_DAYS,
        "hourly_retention_days": maintenance::HOUR_SAMPLES_DAYS,
        "hourly": plan.hourly,
        "bucket_seconds": plan.bucket_seconds,
        "samples": samples,
        "services": services,
    })))
}

fn agent_error(e: &AgentCallError) -> ApiError {
    match e {
        AgentCallError::Unreachable(_) => ApiError::AgentUnavailable,
        AgentCallError::Refused { code, .. } => match code.as_str() {
            "busy" => ApiError::AgentBusy,
            "service_not_allowed" => ApiError::ServiceNotAllowed,
            "service_not_found" => ApiError::NotFound,
            _ => ApiError::ActionFailed,
        },
    }
}

/// `POST /api/admin/services/{service}/restart`: restarts one service of this stack.
///
/// Restarting the platform itself or the reverse proxy cuts the very request that asked for
/// it: for these two the answer is sent first and the restart follows.
pub async fn restart_service(State(state): State<SharedState>, AdminAccount(admin): AdminAccount, Path(service): Path<String>) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::OperatePlatform)?;
    if !RESTARTABLE_SERVICES.contains(&service.as_str()) {
        return Err(ApiError::ServiceNotAllowed);
    }
    if state.monitor.agent().is_none() {
        return Err(ApiError::AgentUnavailable);
    }
    tracing::info!(service, by = %admin.email, "service restart requested");
    if service == "app" || service == "caddy" {
        let details = Details::new().text(Detail::Outcome, "requested");
        audit::record(&state.db, Actor::Account(&admin), AuditAction::ServiceRestarted, Target::object(&service), details).await;
        let state = state.clone();
        tokio::spawn(async move {
            // Leave the answer the time to reach the browser.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            if let Some(agent) = state.monitor.agent() {
                if let Err(e) = agent.restart(&service).await {
                    tracing::error!(service, error = %e, "restart failed");
                }
            }
        });
        return Ok(Json(json!({ "restarted": false, "accepted": true })));
    }
    let agent = state.monitor.agent().ok_or(ApiError::AgentUnavailable)?;
    let outcome = agent.restart(&service).await;
    let details = Details::new().text(Detail::Outcome, if outcome.is_ok() { "ok" } else { "failed" });
    audit::record(&state.db, Actor::Account(&admin), AuditAction::ServiceRestarted, Target::object(&service), details).await;
    state.monitor.refresh_soon();
    match outcome {
        Ok(_) => Ok(Json(json!({ "restarted": true, "accepted": true }))),
        Err(e) => {
            tracing::warn!(service, error = %e, "restart failed");
            Err(agent_error(&e))
        }
    }
}

/// `POST /api/admin/backups/run`: writes a backup archive now. Answers when it is written.
pub async fn run_backup(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::OperatePlatform)?;
    let stored = crate::settings::load(&state.db).await?;
    tracing::info!(by = %admin.email, "backup requested");
    let monitor: &Arc<super::Monitor> = &state.monitor;
    match maintenance::run_backup(monitor, &stored, "manual", Actor::Account(&admin)).await {
        Ok(run) => Ok(Json(json!({ "run": run }))),
        Err(BackupError::NoAgent) => Err(ApiError::AgentUnavailable),
        Err(BackupError::Busy) => Err(ApiError::AgentBusy),
        Err(BackupError::Failed(e)) => {
            tracing::error!(error = %e, "backup request failed");
            Err(ApiError::ActionFailed)
        }
    }
}

/// `POST /api/admin/letsencrypt/sync`: hands Caddy's Let's Encrypt certificate to TAK
/// Server's enrollment port now. TAK Server is restarted only when the certificate changed.
pub async fn letsencrypt_sync(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::OperatePlatform)?;
    let agent = state.monitor.agent().ok_or(ApiError::AgentUnavailable)?;
    tracing::info!(by = %admin.email, "Let's Encrypt hand-over requested");
    let answer = agent.letsencrypt_sync().await;
    let outcome = match &answer {
        Ok(result) => result.outcome.clone(),
        Err(_) => "failed".to_string(),
    };
    let details = Details::new().text(Detail::Trigger, "manual").text(Detail::Outcome, &outcome);
    audit::record(&state.db, Actor::Account(&admin), AuditAction::LetsEncryptSynced, Target::object("takserver"), details).await;
    state.monitor.refresh_soon();
    match answer {
        Ok(result) if result.outcome != "failed" => Ok(Json(json!({ "outcome": result.outcome }))),
        Ok(result) => {
            tracing::warn!(error = ?result.error, "Let's Encrypt hand-over failed");
            Err(ApiError::ActionFailed)
        }
        Err(e) => Err(agent_error(&e)),
    }
}

/// `POST /api/admin/release/check`: asks GitHub for TeamTAK's latest release now, without
/// waiting for the next scheduled check, and answers with what the dashboard shows of it.
/// GitHub's refusal is part of that answer (`error`), as after a scheduled check.
pub async fn check_release(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::ViewMonitoring)?;
    let monitor = &state.monitor;
    if let Err(e) = release::check_now(monitor).await {
        tracing::error!(error = %format!("{e:#}"), "release check requested");
        return Err(ApiError::ActionFailed);
    }
    let check = release::load(&state.db).await?;
    let (view, _) = release::status(monitor.targets.release_repository.as_deref(), &check);
    Ok(Json(json!({ "release": view })))
}

/// `POST /api/admin/settings/webhook-test`: posts a test document to the webhook that is set.
pub async fn webhook_test(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::OperatePlatform)?;
    let settings = crate::settings::load(&state.db).await?.values;
    if settings.webhook_url.is_empty() {
        return Err(ApiError::WebhookNotSet);
    }
    let outcome = state.monitor.post_webhook_test(&settings.webhook_url).await;
    match outcome {
        Ok(status) => Ok(Json(json!({ "status": status }))),
        Err(error) => {
            tracing::warn!(error, "webhook test failed");
            Err(ApiError::ActionFailed)
        }
    }
}

/// `POST /api/admin/settings/mail-test`: sends the test e-mail to the administrator who
/// asks, in their language, and says how it went. A refusal of the mail server is a result
/// of the test, not an error of the request: the answer carries its technical text, the one
/// the monitoring page shows (without addresses).
pub async fn mail_test(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::OperatePlatform)?;
    if !state.mailer.is_configured() {
        return Err(ApiError::MailNotConfigured);
    }
    tracing::info!(by = %admin.email, "test e-mail requested");
    let outcome = state.mailer.send_test(&admin.email, &admin.language).await;
    // The monitoring page shows the last e-mail sent or failed: let it see this one.
    state.monitor.refresh_soon();
    match outcome {
        Ok(()) => Ok(Json(json!({ "sent": true, "to": admin.email, "error": null }))),
        Err(e) => {
            let error = format!("{e:#}");
            tracing::warn!(error, "test e-mail not sent");
            Ok(Json(json!({ "sent": false, "to": admin.email, "error": scrub_error(&error) })))
        }
    }
}

impl super::Monitor {
    /// When the webhook was last posted to, and how it went.
    pub fn webhook_last(&self) -> Option<(OffsetDateTime, Result<u16, String>)> {
        self.webhook_last.lock().unwrap().clone()
    }

    pub async fn post_webhook_test(&self, url: &str) -> Result<u16, String> {
        let http = self.http.as_ref().ok_or("no HTTP client")?;
        let outcome = alerting::post_webhook(http, url, &alerting::webhook_test_payload(&self.targets.public_url)).await;
        *self.webhook_last.lock().unwrap() = Some((OffsetDateTime::now_utc(), outcome.clone()));
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_history_period_stays_within_what_is_kept() {
        use time::macros::datetime;
        use time::Duration;
        let now = datetime!(2026-11-20 12:00 UTC);
        let ask = |hours: Option<i64>, from: Option<i64>, to: Option<i64>| history_period(&HistoryQuery { hours, from, to }, now);
        let at = |t: OffsetDateTime| Some(t.unix_timestamp());
        assert_eq!(ask(None, None, None).unwrap(), (now - Duration::hours(24), now), "a day by default");
        assert_eq!(ask(Some(24 * 180), None, None).unwrap().0, now - Duration::days(180));
        assert_eq!(ask(Some(0), None, None).unwrap().0, now - Duration::hours(1));
        assert_eq!(ask(Some(i64::MAX), None, None).unwrap().0, history::oldest(now), "no further back than the hours are kept");
        let (from, to) = (now - Duration::days(40), now - Duration::days(39));
        assert_eq!(ask(None, at(from), at(to)).unwrap(), (from, to));
        assert_eq!(ask(None, at(now - Duration::days(9000)), at(now + Duration::days(1))).unwrap(), (history::oldest(now), now), "cut to what exists");
        assert_eq!(ask(None, at(to), at(from)), Err(ApiError::InvalidInput), "the end before the start");
        assert_eq!(ask(None, at(now + Duration::hours(1)), at(now + Duration::hours(2))), Err(ApiError::InvalidInput), "all in the future");
        assert_eq!(ask(None, at(from), None), Err(ApiError::InvalidInput), "a start without an end");
        assert_eq!(ask(None, Some(i64::MAX), at(to)), Err(ApiError::InvalidInput));
    }

    #[test]
    fn agent_refusals_become_api_errors() {
        let refused = |code: &str| AgentCallError::Refused { code: code.into(), detail: None };
        assert_eq!(agent_error(&AgentCallError::Unreachable("x".into())), ApiError::AgentUnavailable);
        assert_eq!(agent_error(&refused("busy")), ApiError::AgentBusy);
        assert_eq!(agent_error(&refused("service_not_allowed")), ApiError::ServiceNotAllowed);
        assert_eq!(agent_error(&refused("service_not_found")), ApiError::NotFound);
        assert_eq!(agent_error(&refused("failed")), ApiError::ActionFailed);
    }
}
