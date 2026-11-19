//! What the platform does once a day, from the hour set in the settings (UTC): a backup,
//! the Let's Encrypt hand-over when it is enabled, the housekeeping of TAK Server's logs,
//! and the purge of every table that grows with time.
//!
//! The first three are done by the host agent on the platform's request; without agent
//! they are skipped and the purges still run. Each run is recorded for the dashboard.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use sqlx::PgPool;
use tak_manager_agent::report::{BackupRequest, RotateRequest};
use time::{Date, OffsetDateTime};

use crate::audit::{self, Actor, AuditAction, Detail, Details, Target};
use crate::settings::{Stored, PASSPHRASE_CONTEXT};

use super::agent::AgentCallError;
use super::snapshot::BackupRun;
use super::Monitor;

/// Days resolved alerts and finished backup runs are kept.
const RESOLVED_ALERTS_DAYS: i32 = 90;
const BACKUP_RUNS_DAYS: i32 = 90;
/// Days of history kept for the charts.
pub const METRIC_SAMPLES_DAYS: i32 = 14;
const MAIL_FAILURES_DAYS: i32 = 30;
/// Longer than any client certificate lives.
const CERTIFICATE_NOTICES_DAYS: i32 = 800;

/// True when the daily jobs should run now: the hour has come and they have not run today.
pub fn is_due(now: OffsetDateTime, hour_utc: u8, last_ran_on: Option<Date>) -> bool {
    let now = now.to_offset(time::UtcOffset::UTC);
    now.hour() >= hour_utc && last_ran_on != Some(now.date())
}

/// Starts the daily jobs in the background when they are due.
pub fn run_if_due(monitor: &Arc<Monitor>, stored: &Stored, now: OffsetDateTime) {
    // A cheap first check with what is in memory; the claim in the database decides.
    if now.to_offset(time::UtcOffset::UTC).hour() < stored.values.backup_hour_utc || monitor.daily_running.swap(true, Ordering::SeqCst) {
        return;
    }
    let monitor = monitor.clone();
    let hour = stored.values.backup_hour_utc;
    tokio::spawn(async move {
        if let Err(e) = daily(&monitor, hour).await {
            tracing::error!(error = %format!("{e:#}"), "daily jobs failed");
        }
        monitor.daily_running.store(false, Ordering::SeqCst);
    });
}

async fn daily(monitor: &Arc<Monitor>, hour_utc: u8) -> Result<()> {
    let now = OffsetDateTime::now_utc();
    let (last,): (Option<Date>,) = sqlx::query_as("SELECT daily_ran_on FROM monitor_state WHERE id = 1").fetch_one(&monitor.db).await?;
    if !is_due(now, hour_utc, last) {
        return Ok(());
    }
    // Claimed before anything runs: a job that fails is not tried again every minute of
    // the day. A backup that did not happen is what the "backup too old" alert is for.
    let claimed = sqlx::query("UPDATE monitor_state SET daily_ran_on = $1 WHERE id = 1 AND daily_ran_on IS DISTINCT FROM $1")
        .bind(now.date())
        .execute(&monitor.db)
        .await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    tracing::info!("daily jobs start");
    // Settings as they are now, not as they were when the look that started this began.
    let stored = crate::settings::load(&monitor.db).await?;
    let mut report = serde_json::Map::new();

    if stored.values.backup_enabled {
        let run = run_backup(monitor, &stored, "schedule", Actor::System).await;
        report.insert("backup".into(), match run {
            Ok(run) => json!({ "ok": run.ok, "file": run.file, "size_bytes": run.size_bytes, "error": run.error }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        });
    } else {
        report.insert("backup".into(), json!({ "skipped": "disabled" }));
    }

    let letsencrypt = monitor.latest().and_then(|s| s.letsencrypt.clone()).is_some_and(|l| l.enabled);
    if let (true, Some(agent)) = (letsencrypt, monitor.agent()) {
        let outcome = match agent.letsencrypt_sync().await {
            Ok(result) => result.outcome,
            Err(e) => {
                tracing::warn!(error = %e, "Let's Encrypt hand-over");
                "failed".to_string()
            }
        };
        let details = Details::new().text(Detail::Trigger, "schedule").text(Detail::Outcome, &outcome);
        audit::record(&monitor.db, Actor::System, AuditAction::LetsEncryptSynced, Target::object("takserver"), details).await;
        report.insert("letsencrypt".into(), json!({ "outcome": outcome }));
    }

    if let Some(agent) = monitor.agent() {
        match agent.rotate_logs(&RotateRequest { keep_days: Some(stored.values.log_keep_days) }).await {
            Ok(done) => {
                let details = Details::new()
                    .text(Detail::Trigger, "schedule")
                    .text(Detail::Outcome, "ok")
                    .number(Detail::DeletedFiles, (done.deleted_files + done.rotated_files) as i64)
                    .number(Detail::SizeBytes, done.remaining_bytes as i64);
                audit::record(&monitor.db, Actor::System, AuditAction::LogsRotated, Target::object("takserver"), details).await;
                report.insert("logs".into(), json!(done));
            }
            Err(e) => {
                tracing::warn!(error = %e, "TAK Server log housekeeping");
                report.insert("logs".into(), json!({ "error": e.to_string() }));
            }
        }
    }

    report.insert("purged".into(), purge(&monitor.db, stored.values.audit_retention_days).await?);
    sqlx::query("UPDATE monitor_state SET daily_ran_at = now(), daily_report = $1 WHERE id = 1")
        .bind(Value::Object(report))
        .execute(&monitor.db)
        .await?;
    tracing::info!("daily jobs done");
    monitor.refresh_soon();
    Ok(())
}

/// Deletes what is past its retention in every table that grows with time, and returns how
/// many rows went from each.
pub async fn purge(db: &PgPool, audit_retention_days: u32) -> Result<Value> {
    let mut purged = serde_json::Map::new();
    for (table, sql, days) in [
        ("audit_log", "DELETE FROM audit_log WHERE occurred_at < now() - make_interval(days => $1)", audit_retention_days as i32),
        ("alerts", "DELETE FROM alerts WHERE resolved_at IS NOT NULL AND resolved_at < now() - make_interval(days => $1)", RESOLVED_ALERTS_DAYS),
        ("metric_samples", "DELETE FROM metric_samples WHERE sampled_at < now() - make_interval(days => $1)", METRIC_SAMPLES_DAYS),
        ("backup_runs", "DELETE FROM backup_runs WHERE started_at < now() - make_interval(days => $1)", BACKUP_RUNS_DAYS),
        ("mail_failures", "DELETE FROM mail_failures WHERE occurred_at < now() - make_interval(days => $1)", MAIL_FAILURES_DAYS),
        ("certificate_notices", "DELETE FROM certificate_notices WHERE sent_at < now() - make_interval(days => $1)", CERTIFICATE_NOTICES_DAYS),
    ] {
        let deleted = sqlx::query(sql).bind(days).execute(db).await?.rows_affected();
        purged.insert(table.to_string(), json!(deleted));
    }
    Ok(Value::Object(purged))
}

#[derive(Debug)]
pub enum BackupError {
    /// The platform runs without agent, or it does not answer.
    NoAgent,
    /// Another action of the agent is running.
    Busy,
    Failed(String),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackupError::NoAgent => write!(f, "the host agent is not available"),
            BackupError::Busy => write!(f, "the host agent is busy"),
            BackupError::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// Asks the agent for a backup and records the run, whatever its outcome.
pub async fn run_backup(monitor: &Arc<Monitor>, stored: &Stored, trigger: &'static str, actor: Actor<'_>) -> Result<BackupRun, BackupError> {
    let agent = monitor.agent().ok_or(BackupError::NoAgent)?;
    let fail = |e: sqlx::Error| BackupError::Failed(e.to_string());
    // The passphrase leaves the database only to go to the agent, in the request's body.
    let passphrase = match &stored.passphrase_enc {
        Some(sealed) => Some(monitor.secrets.open_string(PASSPHRASE_CONTEXT, sealed).map_err(|e| BackupError::Failed(format!("the backup passphrase cannot be read: {e}")))?),
        None => None,
    };
    let requested_by = match actor {
        Actor::Account(account) => account.email.clone(),
        _ => String::new(),
    };
    // A run left unfinished by a restart of the platform is closed before a new one starts.
    sqlx::query("UPDATE backup_runs SET ok = false, finished_at = now(), error = 'interrupted' WHERE ok IS NULL AND started_at < now() - interval '45 minutes'")
        .execute(&monitor.db)
        .await
        .map_err(fail)?;
    let (id,): (i64,) = sqlx::query_as("INSERT INTO backup_runs (trigger, requested_by, encrypted) VALUES ($1, $2, $3) RETURNING id")
        .bind(trigger)
        .bind(&requested_by)
        .bind(passphrase.is_some())
        .fetch_one(&monitor.db)
        .await
        .map_err(fail)?;

    let answer = agent.backup(&BackupRequest { passphrase, keep: Some(stored.values.backup_keep) }).await;
    let (ok, file, size, error): (bool, Option<String>, Option<i64>, Option<String>) = match &answer {
        Ok(result) if result.ok => (true, result.file.clone(), result.size_bytes.map(|s| s as i64), None),
        Ok(result) => (false, None, None, Some(result.error.clone().unwrap_or_else(|| "the backup script failed".into()))),
        Err(e) => (false, None, None, Some(e.to_string())),
    };
    let run: BackupRun = sqlx::query_as(
        "UPDATE backup_runs SET finished_at = now(), ok = $2, file = $3, size_bytes = $4, error = $5 WHERE id = $1
         RETURNING id, started_at, finished_at, trigger, requested_by, ok, file, size_bytes, encrypted, error",
    )
    .bind(id)
    .bind(ok)
    .bind(&file)
    .bind(size)
    .bind(&error)
    .fetch_one(&monitor.db)
    .await
    .map_err(fail)?;

    let mut details = Details::new().text(Detail::Trigger, trigger).text(Detail::Outcome, if ok { "ok" } else { "failed" }).flag(Detail::Encrypted, run.encrypted);
    if let (Some(file), Some(size)) = (&file, size) {
        details = details.text(Detail::File, file).number(Detail::SizeBytes, size);
    }
    audit::record(&monitor.db, actor, AuditAction::BackupRun, Target::none(), details).await;
    if ok {
        tracing::info!(trigger, file = ?file, "backup done");
    } else {
        tracing::error!(trigger, error = ?error, "backup failed");
    }
    monitor.refresh_soon();
    match answer {
        Err(AgentCallError::Refused { code, .. }) if code == "busy" => Err(BackupError::Busy),
        Err(AgentCallError::Unreachable(_)) => Err(BackupError::NoAgent),
        _ => Ok(run),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime};

    #[test]
    fn the_daily_jobs_run_once_a_day_from_the_set_hour() {
        let at = |h: u8| datetime!(2026-11-19 0:00 UTC).replace_hour(h).unwrap();
        let today = date!(2026 - 11 - 19);
        let yesterday = date!(2026 - 11 - 18);
        assert!(!is_due(at(2), 3, Some(yesterday)), "before the hour");
        assert!(is_due(at(3), 3, Some(yesterday)));
        assert!(is_due(at(23), 3, Some(yesterday)), "the platform was stopped at 3: later the same day");
        assert!(!is_due(at(4), 3, Some(today)), "already done today");
        assert!(is_due(at(18), 3, None), "a new installation: the first backup does not wait for tomorrow");
        assert!(is_due(at(0), 0, Some(yesterday)));
        // The hour is UTC whatever offset the time carries.
        let paris = datetime!(2026-11-19 03:30 +01:00);
        assert!(!is_due(paris, 3, Some(yesterday)), "02:30 UTC");
    }

    #[test]
    fn the_history_is_bounded_by_design() {
        // One sample every five minutes for the retention: the table cannot hold more.
        let most = METRIC_SAMPLES_DAYS as i64 * 24 * 60 / super::super::SAMPLE_EVERY.whole_minutes();
        assert_eq!(most, 4032);
    }
}
