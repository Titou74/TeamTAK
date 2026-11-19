//! From what was observed to what is said: evaluates the alert rules, records the alerts,
//! and tells people.
//!
//! - Every administrator receives one e-mail per evaluation that has something to say, in
//!   their own language, listing everything raised, resolved and recalled at once.
//! - The same transitions are posted one by one to the webhook, when an address is set.
//! - The leaders of a team are told, in their language, when a certificate shipped in one
//!   of the team's zips nears its end.
//!
//! E-mails and webhook calls leave from a background task: a slow mail server never holds
//! the monitoring loop.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tak_manager_core::alerts::{evaluate, plan, AlertKind, Finding, Observations, OpenAlert, Policy, Severity, Transition};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::locale::{self, AlertBatch, AlertLine, Template};
use crate::settings::Stored;

use super::Monitor;

#[derive(sqlx::FromRow)]
struct OpenRow {
    id: i64,
    kind: String,
    subject: String,
    severity: String,
    details: Value,
    raised_at: OffsetDateTime,
    last_notified_at: Option<OffsetDateTime>,
}

/// What the webhook is told about one alert.
#[derive(Debug, Clone, PartialEq)]
struct WebhookEvent {
    /// `raised`, `escalated`, `reminder` or `resolved`.
    event: &'static str,
    id: i64,
    line: AlertLine,
    raised_at: OffsetDateTime,
    resolved_at: Option<OffsetDateTime>,
}

fn rfc3339(at: OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

fn line(kind: &str, subject: &str, severity: &str, details: Value) -> AlertLine {
    AlertLine { kind: kind.to_string(), subject: subject.to_string(), severity: severity.to_string(), details }
}

fn line_of(finding: &Finding) -> AlertLine {
    line(finding.kind.code(), &finding.subject, finding.severity.as_str(), json!(finding.details))
}

/// The JSON document posted to the webhook for one alert. Kept generic on purpose: stable
/// codes and figures for a program, and one English sentence for a chat room.
///
/// ```json
/// {
///   "source": "tak-manager",
///   "version": 1,
///   "event": "raised",
///   "text": "The service caddy is not running (state: exited).",
///   "url": "https://tak.example.org/monitoring",
///   "alert": {
///     "id": 12, "kind": "service_down", "subject": "caddy", "severity": "critical",
///     "details": { "state": "exited" },
///     "raised_at": "2026-11-19T17:40:12Z", "resolved_at": null
///   }
/// }
/// ```
///
/// `event` is `raised`, `escalated` (the alert became more serious), `reminder`, `resolved`,
/// or `test` (sent from the settings page). `kind` is one of the codes of
/// `tak_manager_core::alerts::AlertKind`; `severity` is `low`, `warning` or `critical`.
fn webhook_payload(event: &WebhookEvent, public_url: &str) -> Value {
    json!({
        "source": "tak-manager",
        "version": 1,
        "event": event.event,
        "text": locale::alert_sentence(locale::DEFAULT_LANGUAGE, &event.line),
        "url": format!("{public_url}/monitoring"),
        "alert": {
            "id": event.id,
            "kind": event.line.kind,
            "subject": event.line.subject,
            "severity": event.line.severity,
            "details": event.line.details,
            "raised_at": rfc3339(event.raised_at),
            "resolved_at": event.resolved_at.map(rfc3339),
        },
    })
}

/// Posts one document to the webhook. Returns the HTTP status, or why it could not be sent.
pub async fn post_webhook(http: &reqwest::Client, url: &str, payload: &Value) -> Result<u16, String> {
    let response = http.post(url).json(payload).send().await.map_err(|e| e.without_url().to_string())?;
    let status = response.status();
    if status.is_success() {
        Ok(status.as_u16())
    } else {
        Err(format!("the webhook answered {}", status.as_u16()))
    }
}

/// The document sent by "Send a test" on the settings page.
pub fn webhook_test_payload(public_url: &str) -> Value {
    json!({
        "source": "tak-manager",
        "version": 1,
        "event": "test",
        "text": "Test message from TAK Manager: alerts will arrive here.",
        "url": format!("{public_url}/monitoring"),
        "alert": null,
    })
}

/// Evaluates the rules on `obs`, records what changed, and notifies.
pub async fn run(monitor: &Arc<Monitor>, stored: &Stored, obs: &Observations) -> Result<()> {
    let settings = &stored.values;
    let now = obs.now;
    let evaluation = evaluate(obs, &settings.thresholds);

    let rows: Vec<OpenRow> = sqlx::query_as(
        "SELECT id, kind, subject, severity, details, raised_at, last_notified_at FROM alerts WHERE resolved_at IS NULL ORDER BY id",
    )
    .fetch_all(&monitor.db)
    .await?;
    let mut open = Vec::new();
    for row in &rows {
        match (AlertKind::parse(&row.kind), Severity::parse(&row.severity)) {
            (Some(kind), Some(severity)) => {
                open.push(OpenAlert { id: row.id, kind, subject: row.subject.clone(), severity, last_notified_at: row.last_notified_at })
            }
            // A kind this version no longer knows: nothing can resolve it, so it is closed.
            _ => {
                sqlx::query("UPDATE alerts SET resolved_at = now() WHERE id = $1").bind(row.id).execute(&monitor.db).await?;
            }
        }
    }

    let policy = Policy { reminder: time::Duration::hours(settings.reminder_hours as i64), ..Policy::default() };
    let transitions = {
        let mut streaks = monitor.streaks.lock().unwrap();
        plan(&open, &evaluation, &mut streaks, &policy, now)
    };

    let mut batch = AlertBatch::default();
    let mut events = Vec::new();
    let raised_at = |id: i64| rows.iter().find(|r| r.id == id).map(|r| r.raised_at).unwrap_or(now);
    for transition in transitions {
        match transition {
            Transition::Raise(finding) => {
                let inserted: Option<(i64,)> = sqlx::query_as(
                    "INSERT INTO alerts (kind, subject, severity, details, raised_at, last_seen_at, last_notified_at)
                     VALUES ($1, $2, $3, $4, $5, $5, $5)
                     ON CONFLICT (kind, subject) WHERE resolved_at IS NULL DO NOTHING RETURNING id",
                )
                .bind(finding.kind.code())
                .bind(&finding.subject)
                .bind(finding.severity.as_str())
                .bind(json!(finding.details))
                .bind(now)
                .fetch_optional(&monitor.db)
                .await?;
                if let Some((id,)) = inserted {
                    tracing::warn!(kind = finding.kind.code(), subject = %finding.subject, severity = finding.severity.as_str(), "alert raised");
                    batch.raised.push(line_of(&finding));
                    events.push(WebhookEvent { event: "raised", id, line: line_of(&finding), raised_at: now, resolved_at: None });
                }
            }
            Transition::Escalate { id, finding } => {
                sqlx::query("UPDATE alerts SET severity = $2, details = $3, last_seen_at = $4, last_notified_at = $4 WHERE id = $1")
                    .bind(id)
                    .bind(finding.severity.as_str())
                    .bind(json!(finding.details))
                    .bind(now)
                    .execute(&monitor.db)
                    .await?;
                tracing::warn!(kind = finding.kind.code(), subject = %finding.subject, severity = finding.severity.as_str(), "alert got worse");
                batch.raised.push(line_of(&finding));
                events.push(WebhookEvent { event: "escalated", id, line: line_of(&finding), raised_at: raised_at(id), resolved_at: None });
            }
            Transition::Refresh { id, finding } => {
                sqlx::query("UPDATE alerts SET severity = $2, details = $3, last_seen_at = $4 WHERE id = $1")
                    .bind(id)
                    .bind(finding.severity.as_str())
                    .bind(json!(finding.details))
                    .bind(now)
                    .execute(&monitor.db)
                    .await?;
            }
            Transition::Remind { id, finding } => {
                sqlx::query("UPDATE alerts SET severity = $2, details = $3, last_seen_at = $4, last_notified_at = $4 WHERE id = $1")
                    .bind(id)
                    .bind(finding.severity.as_str())
                    .bind(json!(finding.details))
                    .bind(now)
                    .execute(&monitor.db)
                    .await?;
                batch.reminded.push(line_of(&finding));
                events.push(WebhookEvent { event: "reminder", id, line: line_of(&finding), raised_at: raised_at(id), resolved_at: None });
            }
            Transition::Resolve { id } => {
                sqlx::query("UPDATE alerts SET resolved_at = $2 WHERE id = $1 AND resolved_at IS NULL").bind(id).bind(now).execute(&monitor.db).await?;
                if let Some(row) = rows.iter().find(|r| r.id == id) {
                    tracing::info!(kind = %row.kind, subject = %row.subject, "alert resolved");
                    let resolved = line(&row.kind, &row.subject, &row.severity, row.details.clone());
                    batch.resolved.push(resolved.clone());
                    events.push(WebhookEvent { event: "resolved", id, line: resolved, raised_at: row.raised_at, resolved_at: Some(now) });
                }
            }
        }
    }
    if batch.is_empty() {
        return Ok(());
    }

    let administrators: Vec<(String, String)> = if settings.alert_emails {
        sqlx::query_as("SELECT email, language FROM accounts WHERE is_admin ORDER BY email").fetch_all(&monitor.db).await?
    } else {
        Vec::new()
    };
    let webhook = Some(settings.webhook_url.clone()).filter(|url| !url.is_empty());
    let monitor = monitor.clone();
    tokio::spawn(async move {
        let url = format!("{}/monitoring", monitor.targets.public_url);
        for (email, language) in administrators {
            let rendered = locale::render_alerts(&language, &batch, &url);
            monitor.mailer.send_rendered_best_effort(&email, Template::AlertDigest, rendered).await;
        }
        if let (Some(webhook), Some(http)) = (webhook, &monitor.http) {
            for event in &events {
                let outcome = post_webhook(http, &webhook, &webhook_payload(event, &monitor.targets.public_url)).await;
                if let Err(error) = &outcome {
                    tracing::warn!(error, event = event.event, kind = %event.line.kind, "webhook not delivered");
                }
                *monitor.webhook_last.lock().unwrap() = Some((OffsetDateTime::now_utc(), outcome.map_err(|e| e.to_string())));
            }
        }
    });
    Ok(())
}

#[derive(sqlx::FromRow)]
struct ExpiringPackage {
    fingerprint: String,
    username: String,
    expires_at: OffsetDateTime,
    team_id: Uuid,
    team_name: String,
}

/// Which notice a certificate is due, given the days it has left: the urgent one within
/// the critical period, the first one within the warning period, none before.
pub fn notice_threshold(days_left: i64, warning_days: i64, critical_days: i64) -> Option<i64> {
    if days_left <= critical_days {
        Some(critical_days)
    } else if days_left <= warning_days {
        Some(warning_days)
    } else {
        None
    }
}

/// Tells the leaders of a team when a certificate shipped in one of its zips nears its end:
/// once when it enters the warning period, once more when it enters the critical one.
/// Nothing renews such a certificate: the access must be regenerated and the zip handed
/// out again.
pub async fn notify_team_leaders(monitor: &Arc<Monitor>, stored: &Stored) -> Result<()> {
    let warning_days = stored.values.thresholds.certificate_warning_days;
    let critical_days = stored.values.thresholds.certificate_critical_days;
    let expiring: Vec<ExpiringPackage> = sqlx::query_as(
        "SELECT c.fingerprint, c.username, c.expires_at, t.id AS team_id, t.name AS team_name
           FROM tak_certificates c JOIN tak_accesses a ON a.id = c.access_id JOIN teams t ON t.id = a.team_id
          WHERE c.source = 'package' AND c.inactive_at IS NULL AND c.revoke_requested_at IS NULL
            AND c.expires_at > now() AND c.expires_at <= now() + make_interval(days => $1 + 1)
            AND t.status IN ('active', 'suspended')
          ORDER BY c.expires_at",
    )
    .bind(warning_days as i32)
    .fetch_all(&monitor.db)
    .await?;
    let now = OffsetDateTime::now_utc();
    for certificate in expiring {
        let days = tak_manager_core::certs::days_left(certificate.expires_at, now);
        let Some(threshold) = notice_threshold(days, warning_days, critical_days) else { continue };
        // The row is the claim: whoever inserts it sends the notice, and nobody sends it twice.
        let claimed = sqlx::query("INSERT INTO certificate_notices (fingerprint, threshold_days) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(&certificate.fingerprint)
            .bind(threshold as i32)
            .execute(&monitor.db)
            .await?;
        if claimed.rows_affected() == 0 {
            continue;
        }
        let leaders: Vec<(String, String)> = sqlx::query_as(
            "SELECT a.email, a.language FROM team_members m JOIN accounts a ON a.id = m.account_id WHERE m.team_id = $1 AND m.role = 'leader'",
        )
        .bind(certificate.team_id)
        .fetch_all(&monitor.db)
        .await?;
        tracing::info!(username = %certificate.username, team = %certificate.team_name, days, leaders = leaders.len(), "certificate expiry told to the team's leaders");
        let monitor = monitor.clone();
        tokio::spawn(async move {
            let url = format!("{}/teams/{}", monitor.targets.public_url, certificate.team_id);
            let days = days.to_string();
            for (email, language) in leaders {
                let date = locale::format_datetime(&language, certificate.expires_at);
                let vars = [
                    ("team", certificate.team_name.as_str()),
                    ("access", certificate.username.as_str()),
                    ("days", days.as_str()),
                    ("date", date.as_str()),
                    ("url", url.as_str()),
                ];
                monitor.mailer.send_best_effort(&email, &language, Template::CertificateNotice, &vars).await;
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn the_webhook_document_is_generic_and_self_describing() {
        let event = WebhookEvent {
            event: "raised",
            id: 12,
            line: line("service_down", "caddy", "critical", json!({ "state": "exited" })),
            raised_at: datetime!(2026-11-19 17:40:12 UTC),
            resolved_at: None,
        };
        let payload = webhook_payload(&event, "https://tak.example.org");
        assert_eq!(
            payload,
            json!({
                "source": "tak-manager",
                "version": 1,
                "event": "raised",
                "text": "The service caddy is not running (state: exited).",
                "url": "https://tak.example.org/monitoring",
                "alert": {
                    "id": 12, "kind": "service_down", "subject": "caddy", "severity": "critical",
                    "details": { "state": "exited" },
                    "raised_at": "2026-11-19T17:40:12Z", "resolved_at": null
                }
            })
        );
        let resolved = WebhookEvent { event: "resolved", resolved_at: Some(datetime!(2026-11-19 17:45 UTC)), ..event };
        assert_eq!(webhook_payload(&resolved, "u")["alert"]["resolved_at"], "2026-11-19T17:45:00Z");
        assert_eq!(webhook_test_payload("https://tak.example.org")["event"], "test");
    }

    #[tokio::test]
    async fn the_webhook_receives_a_json_post() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hooks/tak", listener.local_addr().unwrap());
        let received = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = vec![0u8; 4096];
            // Headers, then the body announced by Content-Length.
            loop {
                let n = stream.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..n]);
                let text = String::from_utf8_lossy(&request).into_owned();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length: usize = head.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        let http = tak_manager_core::public_http_client(std::time::Duration::from_secs(5)).unwrap();
        let event = WebhookEvent {
            event: "raised",
            id: 3,
            line: line("disk_full", "root", "warning", json!({ "percent": 91.5 })),
            raised_at: datetime!(2026-11-19 12:00 UTC),
            resolved_at: None,
        };
        assert_eq!(post_webhook(&http, &url, &webhook_payload(&event, "https://tak.example.org")).await, Ok(204));
        let request = received.await.unwrap();
        assert!(request.starts_with("POST /hooks/tak HTTP/1.1"), "{request}");
        assert!(request.to_lowercase().contains("content-type: application/json"));
        let body: Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!((body["alert"]["kind"].clone(), body["alert"]["details"]["percent"].clone()), (json!("disk_full"), json!(91.5)));
        assert_eq!(body["text"], "Disk nearly full: the system disk is 91.5 % used.");
    }

    #[tokio::test]
    async fn a_webhook_that_refuses_is_reported_not_retried() {
        let http = tak_manager_core::public_http_client(std::time::Duration::from_secs(2)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let error = post_webhook(&http, &url, &json!({})).await.unwrap_err();
        assert!(!error.contains("127.0.0.1"), "the address (which may hold a secret path) is not repeated: {error}");
    }

    #[test]
    fn leaders_are_told_at_thirty_and_at_seven_days() {
        assert_eq!(notice_threshold(45, 30, 7), None);
        assert_eq!(notice_threshold(30, 30, 7), Some(30));
        assert_eq!(notice_threshold(8, 30, 7), Some(30));
        assert_eq!(notice_threshold(7, 30, 7), Some(7));
        assert_eq!(notice_threshold(0, 30, 7), Some(7));
        // Other thresholds chosen by the administrator.
        assert_eq!(notice_threshold(50, 60, 14), Some(60));
        assert_eq!(notice_threshold(14, 60, 14), Some(14));
    }
}
