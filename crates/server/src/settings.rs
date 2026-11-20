//! Settings an administrator changes from the web UI: the name of the server, alert
//! thresholds, where alerts go, the backup schedule, retention periods, and the latest TAK
//! Server version they saw on tak.gov. Stored as one row; a field missing from the stored JSON takes its default, so a
//! new setting needs no migration.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use teamtak_core::alerts::Thresholds;
use teamtak_core::policy::Action;
use teamtak_core::versions::parse_tak_version;
use time::OffsetDateTime;

use crate::audit::{self, Actor, AuditAction, Detail, Details, Target};
use crate::auth::AdminAccount;
use crate::error::{ApiError, ApiResult};
use crate::state::SharedState;

/// Context of the passphrase column, authenticated with the ciphertext.
pub const PASSPHRASE_CONTEXT: &str = "settings.backup_passphrase";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Shown on the sign-in page, under the brand and in the subject of the e-mails, to tell
    /// this server from the others an organisation runs. Empty for none.
    pub server_name: String,
    pub thresholds: Thresholds,
    /// Alerts are e-mailed to every administrator.
    pub alert_emails: bool,
    /// Alerts are also posted there as JSON; empty for none.
    pub webhook_url: String,
    /// An alert that lasts is recalled at most once per this many hours.
    pub reminder_hours: u32,
    pub backup_enabled: bool,
    /// Hour of the day, UTC, from which the daily backup and housekeeping run.
    pub backup_hour_utc: u8,
    /// Archives kept in the backup folder.
    pub backup_keep: u32,
    pub audit_retention_days: u32,
    /// Days of rolled TAK Server logs kept.
    pub log_keep_days: u32,
    /// Latest TAK Server version the administrator saw on tak.gov, which has no public API
    /// to ask. Empty when none was recorded.
    pub tak_latest_known_version: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            server_name: String::new(),
            thresholds: Thresholds::default(),
            alert_emails: true,
            webhook_url: String::new(),
            reminder_hours: 24,
            backup_enabled: true,
            backup_hour_utc: 3,
            backup_keep: 14,
            audit_retention_days: 365,
            log_keep_days: 30,
            tak_latest_known_version: String::new(),
        }
    }
}

/// Longest server name accepted, in characters.
const SERVER_NAME_MAX: usize = 60;

/// Longest passphrase accepted, and shortest: the archive holds every secret of the platform.
const PASSPHRASE_LENGTH: std::ops::RangeInclusive<usize> = 12..=200;

impl Settings {
    /// Checks what cannot be clamped and brings the rest within reasonable bounds.
    pub fn validated(mut self) -> Result<Self, ApiError> {
        // Runs of spaces are one: the name is shown on one line.
        self.server_name = self.server_name.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.server_name.chars().count() > SERVER_NAME_MAX || self.server_name.chars().any(char::is_control) {
            return Err(ApiError::InvalidInput);
        }
        self.thresholds = self.thresholds.sanitized();
        self.webhook_url = self.webhook_url.trim().to_string();
        if !self.webhook_url.is_empty() && !is_webhook_url(&self.webhook_url) {
            return Err(ApiError::InvalidInput);
        }
        self.tak_latest_known_version = self.tak_latest_known_version.trim().to_string();
        if !self.tak_latest_known_version.is_empty() {
            // Stored the way tak.gov writes it, whatever form was typed.
            self.tak_latest_known_version = parse_tak_version(&self.tak_latest_known_version).ok_or(ApiError::InvalidInput)?.to_string();
        }
        if self.backup_hour_utc > 23 {
            return Err(ApiError::InvalidInput);
        }
        self.reminder_hours = self.reminder_hours.clamp(1, 24 * 14);
        self.backup_keep = self.backup_keep.clamp(1, 365);
        self.audit_retention_days = self.audit_retention_days.clamp(30, 3650);
        self.log_keep_days = self.log_keep_days.clamp(1, 365);
        Ok(self)
    }
}

/// An `http://` or `https://` address with a host, short enough, without credentials in it.
fn is_webhook_url(url: &str) -> bool {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    let Some(rest) = rest else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= 500 && !authority.is_empty() && !authority.contains('@') && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub struct Stored {
    pub values: Settings,
    /// The backup passphrase, still encrypted.
    pub passphrase_enc: Option<String>,
    /// When the platform was installed (or upgraded to the version that has settings).
    pub created_at: OffsetDateTime,
}

pub async fn load(db: &PgPool) -> Result<Stored, sqlx::Error> {
    let (data, passphrase_enc, created_at): (Value, Option<String>, OffsetDateTime) =
        sqlx::query_as("SELECT data, backup_passphrase_enc, created_at FROM settings WHERE id = 1").fetch_one(db).await?;
    // Unreadable or out-of-range stored values fall back to the defaults rather than stop
    // the monitoring.
    let values = serde_json::from_value::<Settings>(data).ok().and_then(|s| s.validated().ok()).unwrap_or_default();
    Ok(Stored { values, passphrase_enc, created_at })
}

fn view(stored: &Stored) -> Value {
    json!({
        "settings": stored.values,
        "defaults": Settings::default(),
        // Whether a passphrase is set; the passphrase itself never leaves the server.
        "backup_passphrase_set": stored.passphrase_enc.is_some(),
    })
}

/// `GET /api/admin/settings`
pub async fn get(State(state): State<SharedState>, AdminAccount(admin): AdminAccount) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::ViewMonitoring)?;
    Ok(Json(view(&load(&state.db).await?)))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    /// The fields to change; the others keep their value.
    #[serde(default)]
    settings: Option<Value>,
    /// A new passphrase for the backup archives; an empty text removes it.
    #[serde(default)]
    backup_passphrase: Option<String>,
}

/// Names of the top-level fields (and thresholds) that differ between two settings.
fn changed_fields(before: &Settings, after: &Settings) -> Vec<String> {
    let (Value::Object(old), Value::Object(new)) = (json!(before), json!(after)) else { return Vec::new() };
    let mut changed = Vec::new();
    for (key, value) in &new {
        match (value, old.get(key)) {
            (Value::Object(inner), Some(Value::Object(old_inner))) => {
                changed.extend(inner.iter().filter(|(k, v)| old_inner.get(*k) != Some(*v)).map(|(k, _)| format!("{key}.{k}")));
            }
            (value, previous) if previous != Some(value) => changed.push(key.clone()),
            _ => {}
        }
    }
    changed
}

/// Lays the fields of `patch` over `base`, one level into objects.
fn merged(base: &Settings, patch: &Value) -> Result<Settings, ApiError> {
    let Value::Object(patch) = patch else { return Err(ApiError::InvalidInput) };
    let Value::Object(mut whole) = json!(base) else { return Err(ApiError::Internal) };
    for (key, value) in patch {
        let Some(current) = whole.get_mut(key) else { return Err(ApiError::InvalidInput) };
        match (current, value) {
            (Value::Object(current), Value::Object(inner)) => {
                for (k, v) in inner {
                    if !current.contains_key(k) {
                        return Err(ApiError::InvalidInput);
                    }
                    current.insert(k.clone(), v.clone());
                }
            }
            (current, value) => *current = value.clone(),
        }
    }
    serde_json::from_value::<Settings>(Value::Object(whole)).map_err(|_| ApiError::InvalidInput)?.validated()
}

/// `PATCH /api/admin/settings`
pub async fn update(State(state): State<SharedState>, AdminAccount(admin): AdminAccount, Json(body): Json<UpdateBody>) -> ApiResult<Json<Value>> {
    admin.require_platform(Action::OperatePlatform)?;
    let stored = load(&state.db).await?;
    let values = match &body.settings {
        Some(patch) => merged(&stored.values, patch)?,
        None => stored.values.clone(),
    };
    let mut changed = changed_fields(&stored.values, &values);

    let passphrase_enc = match body.backup_passphrase.as_deref() {
        None => stored.passphrase_enc.clone(),
        Some("") => None,
        Some(passphrase) => {
            if !PASSPHRASE_LENGTH.contains(&passphrase.chars().count()) || passphrase.chars().any(char::is_control) {
                return Err(ApiError::InvalidInput);
            }
            Some(state.secrets.seal(PASSPHRASE_CONTEXT, passphrase.as_bytes()))
        }
    };
    if body.backup_passphrase.is_some() && (passphrase_enc.is_some() || stored.passphrase_enc.is_some()) {
        changed.push("backup_passphrase".to_string());
    }

    sqlx::query("UPDATE settings SET data = $1, backup_passphrase_enc = $2, updated_at = now(), updated_by = $3 WHERE id = 1")
        .bind(json!(values))
        .bind(&passphrase_enc)
        .bind(&admin.email)
        .execute(&state.db)
        .await?;
    if !changed.is_empty() {
        tracing::info!(by = %admin.email, changed = %changed.join(", "), "settings changed");
        let details = Details::new().text(Detail::Settings, &changed.join(", "));
        audit::record(&state.db, Actor::Account(&admin), AuditAction::SettingsChanged, Target::none(), details).await;
        // Thresholds apply at the next look.
        state.monitor.refresh_soon();
    }
    Ok(Json(view(&load(&state.db).await?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sensible_and_valid() {
        let d = Settings::default();
        assert!(d.backup_enabled && d.alert_emails);
        assert_eq!((d.backup_hour_utc, d.backup_keep, d.audit_retention_days, d.reminder_hours), (3, 14, 365, 24));
        assert_eq!(d.clone().validated().unwrap(), d);
        // An empty stored document is the defaults.
        assert_eq!(serde_json::from_value::<Settings>(json!({})).unwrap(), d);
    }

    #[test]
    fn a_change_touches_only_the_fields_it_names() {
        let base = Settings::default();
        let after = merged(&base, &json!({ "backup_keep": 30, "thresholds": { "disk_percent": 80.0 } })).unwrap();
        assert_eq!(after.backup_keep, 30);
        assert_eq!(after.thresholds.disk_percent, 80.0);
        assert_eq!(after.thresholds.memory_percent, base.thresholds.memory_percent);
        assert_eq!(changed_fields(&base, &after), vec!["backup_keep".to_string(), "thresholds.disk_percent".to_string()]);
        assert_eq!(changed_fields(&base, &base), Vec::<String>::new());
    }

    #[test]
    fn unknown_fields_and_wrong_types_are_refused() {
        let base = Settings::default();
        for patch in [
            json!({ "no_such_setting": 1 }),
            json!({ "thresholds": { "no_such_threshold": 1 } }),
            json!({ "backup_keep": "many" }),
            json!({ "backup_hour_utc": 24 }),
            json!({ "webhook_url": "ftp://example.org/hook" }),
            json!({ "webhook_url": "https://user:secret@example.org/hook" }),
            json!({ "tak_latest_known_version": "the new one" }),
            json!({ "server_name": "x".repeat(61) }),
            json!({ "server_name": "two\u{0}parts" }),
            json!({ "server_name": 12 }),
            json!([1, 2]),
        ] {
            assert_eq!(merged(&base, &patch), Err(ApiError::InvalidInput), "{patch}");
        }
    }

    #[test]
    fn values_are_brought_within_bounds() {
        let base = Settings::default();
        let after = merged(&base, &json!({ "backup_keep": 0, "audit_retention_days": 1, "reminder_hours": 100000, "thresholds": { "disk_percent": 10.0 } })).unwrap();
        assert_eq!((after.backup_keep, after.audit_retention_days, after.reminder_hours), (1, 30, 336));
        assert_eq!(after.thresholds.disk_percent, 50.0);
    }

    #[test]
    fn the_latest_known_tak_version_is_normalised() {
        let base = Settings::default();
        let after = merged(&base, &json!({ "tak_latest_known_version": " 5.9-release-12 " })).unwrap();
        assert_eq!(after.tak_latest_known_version, "5.9-RELEASE-12");
        assert_eq!(merged(&after, &json!({ "tak_latest_known_version": "" })).unwrap().tak_latest_known_version, "");
    }

    #[test]
    fn the_server_name_is_tidied() {
        let base = Settings::default();
        assert_eq!(base.server_name, "");
        let after = merged(&base, &json!({ "server_name": "  Exercise   North\t(tak.example.org) " })).unwrap();
        assert_eq!(after.server_name, "Exercise North (tak.example.org)");
        assert_eq!(merged(&base, &json!({ "server_name": "é".repeat(60) })).unwrap().server_name.chars().count(), 60);
        assert_eq!(merged(&after, &json!({ "server_name": "   " })).unwrap().server_name, "");
        assert_eq!(changed_fields(&base, &after), vec!["server_name".to_string()]);
    }

    #[test]
    fn webhook_addresses() {
        for ok in ["https://hooks.example.org/services/T0/B0/xyz", "http://ntfy.internal:8080/tak", "https://example.org"] {
            assert!(is_webhook_url(ok), "{ok}");
        }
        for bad in ["", "example.org/hook", "https://", "https:///path", "javascript:alert(1)", "https://example.org/a b", "https://u:p@example.org/"] {
            assert!(!is_webhook_url(bad), "{bad}");
        }
    }
}
