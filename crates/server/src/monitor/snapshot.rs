//! What the dashboard shows: the latest look at the machine, the stack, TAK Server, the
//! certificates and the platform itself. Built by the monitoring loop, kept in memory, and
//! served as it is: no request of the web UI triggers a probe.

use serde::Serialize;
use teamtak_agent::report::{BackupFile, HostReport, LetsEncryptReport, ProcessReport};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    #[serde(with = "time::serde::rfc3339")]
    pub collected_at: OffsetDateTime,
    pub interval_seconds: u64,
    pub agent: AgentView,
    /// `local` or `production`, as the agent reads it; empty without agent.
    pub mode: String,
    /// The machine; `None` when the agent does not answer.
    pub host: Option<HostReport>,
    pub services: ServicesView,
    pub tak: TakView,
    pub certificates: Vec<CertificateView>,
    pub client_certificates: ClientCertificatesView,
    pub platform: PlatformView,
    pub backups: BackupsView,
    pub letsencrypt: Option<LetsEncryptReport>,
    pub devices: DeviceCounts,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    /// `ok`, `unreachable`, or `disabled` when the platform was started without an agent.
    pub status: &'static str,
    pub error: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServicesView {
    pub services: Vec<ServiceView>,
    /// Why the containers could not be listed, when the agent answered but Docker did not.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ServiceView {
    pub service: String,
    /// False when the stack should have this service and has no container for it.
    pub present: bool,
    pub state: Option<String>,
    pub health: Option<String>,
    pub restart_count: u32,
    /// Restarts counted over the last quarter of an hour.
    pub recent_restarts: u32,
    pub started_at: Option<String>,
    pub image: String,
    pub image_created: Option<String>,
    /// An administrator may restart it from the web UI.
    pub restartable: bool,
    pub update: ImageUpdate,
}

/// Whether a newer image exists, and what "newer" means for this image.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImageUpdate {
    /// `local`: built on this machine, no registry to ask.
    /// `tag`: pulled by tag; newer means the registry now serves another image under it.
    /// `digest`: pinned by digest on purpose; the registry's `latest` is compared for
    /// information.
    pub source: &'static str,
    /// `local`, `up_to_date`, `update_available`, `pinned`, `pinned_newer_exists`, `unknown`.
    pub status: &'static str,
    pub checked_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TakView {
    pub ports: Vec<PortView>,
    /// The API answers the platform's certificate.
    pub api_ok: bool,
    /// Version the running server reports.
    pub version: Option<String>,
    /// Version the images were built from.
    pub installed_version: String,
    /// TAK Server ZIPs at the repository root.
    pub zip_versions: Vec<String>,
    /// Among them, those of a newer version than the installed one: an upgrade is at hand.
    pub other_zip_versions: Vec<String>,
    /// What the administrator last saw on tak.gov.
    pub latest_known_version: String,
    /// That version is newer than the installed one.
    pub newer_known: bool,
    pub processes: Vec<ProcessReport>,
    pub processes_error: Option<String>,
    pub logs_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PortView {
    pub port: u16,
    /// `clients`, `api` or `enrollment`.
    pub role: &'static str,
    pub open: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CertificateView {
    /// `root_ca`, `intermediate_ca`, `tak_server`, `platform_api`, `enrollment`, `web`.
    pub id: &'static str,
    pub subject: String,
    pub issuer: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub days_left: i64,
    /// `ok`, `warning`, `critical` or `expired`.
    pub level: &'static str,
    /// `file` (read from the platform's files) or `presented` (shown by the server on its port).
    pub from: &'static str,
    /// The port did not answer this time: this is the certificate it presented earlier.
    pub stale: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientCertificatesView {
    pub active: i64,
    /// Expiring within `warning_days`, and within `critical_days`.
    pub expiring_warning: i64,
    pub expiring_critical: i64,
    pub warning_days: i64,
    pub critical_days: i64,
    pub soonest: Vec<ClientCertificate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientCertificate {
    pub username: String,
    pub team_id: Uuid,
    pub team_name: String,
    /// `package` (shipped in a zip) or `device` (enrolled by a device).
    pub source: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    pub days_left: i64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PlatformView {
    pub reconcile: ReconcileView,
    pub drift_24h: i64,
    pub drift_7d: i64,
    /// Starts and ends of events that no pass has applied yet.
    pub overdue_events: Vec<OverdueEventView>,
    pub mail: MailView,
    /// The directory accepts connections.
    pub directory_open: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    /// TeamTAK's own version, and the latest one published.
    pub release: ReleaseView,
}

/// The version that runs and the latest release on GitHub.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ReleaseView {
    pub installed: &'static str,
    /// `owner/name` of the GitHub repository asked; `None` when the check is turned off.
    pub repository: Option<String>,
    /// `up_to_date`, `update_available`, `no_release` (none published yet), `unknown`
    /// (GitHub has not answered yet) or `off`.
    pub status: &'static str,
    pub latest: Option<LatestRelease>,
    /// When GitHub was last asked, and why it did not answer then.
    pub checked_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatestRelease {
    /// `1.0.2`, read from the tag; the tag itself when it is not a version.
    pub version: String,
    pub tag: String,
    pub name: String,
    /// Its page on GitHub, with the release notes.
    pub url: String,
    pub published_at: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ReconcileView {
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_run_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_ok_at: Option<OffsetDateTime>,
    /// The time the last clean pass computed the groups for: every start and end of an
    /// event up to it has been applied.
    #[serde(with = "time::serde::rfc3339::option")]
    pub desired_at: Option<OffsetDateTime>,
    pub directory_reachable: bool,
    pub tak_reachable: bool,
    pub errors: Vec<String>,
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverdueEventView {
    pub group: String,
    pub name: String,
    /// `start` or `end`.
    pub boundary: &'static str,
    #[serde(with = "time::serde::rfc3339")]
    pub due_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MailView {
    /// False when no mail server is set: nothing can be sent.
    pub configured: bool,
    pub failures_last_hour: i64,
    pub failures_24h: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_failure_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_success_at: Option<OffsetDateTime>,
    /// Technical text of the latest failure, as the mail library gave it (English).
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BackupsView {
    pub enabled: bool,
    pub hour_utc: u8,
    pub keep: u32,
    pub passphrase_set: bool,
    pub max_age_hours: i64,
    /// Archives in the backup folder, newest first; empty without agent.
    pub files: Vec<BackupFile>,
    pub total_bytes: u64,
    pub running: bool,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub newest_at: Option<OffsetDateTime>,
    pub runs: Vec<BackupRun>,
    /// When the daily jobs last ran, and what they did.
    #[serde(with = "time::serde::rfc3339::option")]
    pub daily_ran_at: Option<OffsetDateTime>,
    pub daily_report: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct BackupRun {
    pub id: i64,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub trigger: String,
    pub requested_by: String,
    pub ok: Option<bool>,
    pub file: Option<String>,
    pub size_bytes: Option<i64>,
    pub encrypted: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct DeviceCounts {
    /// Sessions TAK Server lists with their account.
    pub identified: usize,
    /// Sessions without any group, which it lists without account.
    pub unidentified: usize,
}

/// `ok`, `warning`, `critical` or `expired` for a number of days left.
pub fn expiry_level(days_left: i64, warning_days: i64, critical_days: i64) -> &'static str {
    if days_left < 0 {
        "expired"
    } else if days_left <= critical_days {
        "critical"
    } else if days_left <= warning_days {
        "warning"
    } else {
        "ok"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_levels_follow_the_thresholds() {
        assert_eq!(expiry_level(400, 30, 7), "ok");
        assert_eq!(expiry_level(31, 30, 7), "ok");
        assert_eq!(expiry_level(30, 30, 7), "warning");
        assert_eq!(expiry_level(8, 30, 7), "warning");
        assert_eq!(expiry_level(7, 30, 7), "critical");
        assert_eq!(expiry_level(0, 30, 7), "critical", "expires today");
        assert_eq!(expiry_level(-1, 30, 7), "expired");
    }
}
