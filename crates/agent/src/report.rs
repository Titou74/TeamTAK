//! What the agent reports and answers. These types are the contract between the agent and
//! the platform: the agent serialises them, the server reads them back.
//!
//! A part the agent could not read is reported as unknown (`None`, or an `error` text),
//! never guessed: the dashboard shows "unknown" rather than a reassuring default.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// Services of the Compose project the agent knows. Nothing outside this list can be
/// named in a request, whatever runs on the same Docker.
pub const CORE_SERVICES: &[&str] = &["caddy", "app", "postgres", "openldap", "takserver", "takserver-db", "agent"];

/// Present in local mode only (the mail catcher).
pub const LOCAL_SERVICES: &[&str] = &["mailpit"];

/// Present when the video server is turned on (`VIDEO=on` in deploy/.env).
pub const VIDEO_SERVICES: &[&str] = &["mediamtx"];

/// Services an administrator may restart from the web UI. The agent itself is not in it: a
/// process cannot reliably answer a request that kills it.
pub const RESTARTABLE_SERVICES: &[&str] =
    &["caddy", "app", "postgres", "openldap", "takserver", "takserver-db", "mailpit", "mediamtx"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub agent_version: String,
    #[serde(with = "time::serde::rfc3339")]
    pub generated_at: OffsetDateTime,
    /// `local` or `production`, from deploy/.env; empty when it could not be read.
    pub mode: String,
    pub host: HostReport,
    pub containers: ContainersReport,
    pub tak: TakReport,
    pub backups: BackupsReport,
    pub letsencrypt: LetsEncryptReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HostReport {
    pub os: OsInfo,
    pub kernel: Option<String>,
    pub uptime_seconds: Option<u64>,
    pub cpu: CpuReport,
    pub memory: Option<MemoryReport>,
    pub disks: Vec<DiskReport>,
    pub clock: ClockReport,
    pub updates: UpdatesReport,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OsInfo {
    /// `ID` of /etc/os-release, lowercase (`debian`, `ubuntu`); empty when unknown.
    pub id: String,
    pub id_like: Vec<String>,
    pub version_id: String,
    pub pretty_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CpuReport {
    pub cores: Option<u32>,
    /// Share of CPU time spent working since the previous report, 0 to 100.
    pub percent: Option<f32>,
    pub load1: Option<f32>,
    pub load5: Option<f32>,
    pub load15: Option<f32>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryReport {
    pub total_bytes: u64,
    /// What the kernel says can be given to programs without swapping (`MemAvailable`).
    pub available_bytes: u64,
    pub swap_total_bytes: u64,
    pub swap_free_bytes: u64,
}

impl MemoryReport {
    pub fn used_percent(&self) -> f32 {
        percent(self.total_bytes.saturating_sub(self.available_bytes), self.total_bytes)
    }

    /// `None` when the machine has no swap.
    pub fn swap_used_percent(&self) -> Option<f32> {
        (self.swap_total_bytes > 0).then(|| percent(self.swap_total_bytes.saturating_sub(self.swap_free_bytes), self.swap_total_bytes))
    }
}

pub fn percent(part: u64, whole: u64) -> f32 {
    if whole == 0 {
        0.0
    } else {
        (part as f64 * 100.0 / whole as f64) as f32
    }
}

/// One filesystem. Several places the platform cares about are often on the same one: it is
/// then reported once, under the first name, with the others in `also`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DiskReport {
    /// `root`, `docker` (Docker's data), `data` (deploy/data) or `backups`.
    pub name: String,
    pub also: Vec<String>,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ClockReport {
    /// What the kernel says (`adjtimex`): true once an NTP client disciplines the clock.
    pub synchronized: Option<bool>,
    /// The kernel's bound on the clock error, in milliseconds.
    pub max_error_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UpdatesReport {
    /// `apt` on Debian and Ubuntu; `None` elsewhere: nothing below is known then.
    pub manager: Option<String>,
    /// Packages with a newer version in the lists the host last downloaded.
    pub pending: Option<u32>,
    /// Those that come from a security archive.
    pub security: Option<u32>,
    /// How old those lists are: the agent never refreshes them, the host does.
    pub lists_age_seconds: Option<u64>,
    pub reboot_required: Option<bool>,
    /// Packages that asked for the reboot, when the host says so.
    pub reboot_packages: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContainersReport {
    /// Services this stack is made of in the current mode.
    pub expected: Vec<String>,
    pub containers: Vec<ContainerReport>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContainerReport {
    /// Name of the Compose service.
    pub service: String,
    pub name: String,
    /// Docker's state: `running`, `exited`, `restarting`, `paused`, `created`, `dead`.
    pub state: String,
    /// `healthy`, `unhealthy` or `starting`; `None` for a service without a healthcheck.
    pub health: Option<String>,
    pub restart_count: u32,
    pub exit_code: Option<i64>,
    pub started_at: Option<String>,
    /// The image as the Compose file names it.
    pub image: String,
    pub image_id: String,
    pub image_created: Option<String>,
    /// `repository@sha256:…` of the image when it was pulled from a registry; empty for an
    /// image built on this machine.
    pub repo_digests: Vec<String>,
    /// Share of the whole machine's processor time it used since the previous report, 0 to
    /// 100, all cores together like the machine's own figure. `None` when it is not running
    /// or its control group could not be read.
    #[serde(default)]
    pub cpu_percent: Option<f32>,
    /// Memory it uses, the file cache the kernel can take back left out: what `docker stats`
    /// shows.
    #[serde(default)]
    pub memory_bytes: Option<u64>,
    /// Space its data takes on disk: its volumes and what it mounts from deploy/data. `None`
    /// for a service without data of its own, and for the agent, whose mounts are the
    /// machine's and the other services'.
    #[serde(default)]
    pub disk_bytes: Option<u64>,
    /// Ports of the machine its configuration publishes (`HostConfig.PortBindings`), stopped
    /// or not: Docker does not start a container whose ports it cannot publish. `None` from
    /// an agent older than this field.
    #[serde(default)]
    pub published_ports: Option<Vec<PublishedPort>>,
}

/// A port of the machine that leads to a port of a container.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PublishedPort {
    /// The container's port it leads to.
    pub container_port: u16,
    /// `tcp` or `udp`.
    pub protocol: String,
    /// The port of the machine.
    pub host_port: u16,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TakReport {
    /// `TAK_VERSION` of deploy/.env: the version the images were built from.
    pub installed_version: String,
    /// Versions of the TAK Server ZIPs found at the repository root.
    pub zip_versions: Vec<String>,
    /// The five Java processes of TAK Server; empty when they could not be listed.
    pub processes: Vec<ProcessReport>,
    pub processes_error: Option<String>,
    /// Size of deploy/data/tak/logs.
    pub logs_bytes: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProcessReport {
    pub name: String,
    pub running: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BackupsReport {
    pub files: Vec<BackupFile>,
    pub total_bytes: u64,
    /// A backup is being written right now.
    pub running: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BackupFile {
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: Option<String>,
    pub encrypted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LetsEncryptReport {
    /// `TAK_ENROLL_LETSENCRYPT=true` in deploy/.env.
    pub enabled: bool,
    /// Caddy holds a certificate for the public host.
    pub certificate_found: bool,
    /// TAK Server's keystore was made from that very certificate.
    pub handed_over: bool,
}

// ------------------------------------------------------------------------------- actions

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupRequest {
    /// Encrypts the archive when present and not empty. Never logged, never echoed.
    #[serde(default)]
    pub passphrase: Option<String>,
    /// Archives to keep; older ones are deleted after a successful backup.
    #[serde(default)]
    pub keep: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BackupResult {
    pub ok: bool,
    pub file: Option<String>,
    pub size_bytes: Option<u64>,
    pub encrypted: bool,
    pub duration_seconds: u64,
    /// Archives deleted by the retention rule.
    pub pruned: u32,
    /// Last lines of the script's output when it failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RestartResult {
    pub service: String,
    pub container: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LetsEncryptResult {
    /// `unchanged`, `updated` (TAK Server was restarted) or `failed`.
    pub outcome: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RotateRequest {
    /// Days of rolled TAK Server logs to keep.
    #[serde(default)]
    pub keep_days: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RotateResult {
    pub deleted_files: u32,
    pub freed_bytes: u64,
    /// Live logs that had grown past the limit and were compressed aside.
    pub rotated_files: u32,
    pub remaining_bytes: u64,
}

/// Body of every refusal. `error` is a stable code.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentError {
    pub error: String,
    #[serde(default)]
    pub detail: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_never_offers_to_restart_itself_or_an_unknown_service() {
        assert!(!RESTARTABLE_SERVICES.contains(&"agent"));
        for service in RESTARTABLE_SERVICES {
            assert!(
                CORE_SERVICES.contains(service) || LOCAL_SERVICES.contains(service) || VIDEO_SERVICES.contains(service),
                "{service}"
            );
        }
    }

    #[test]
    fn memory_shares() {
        let m = MemoryReport { total_bytes: 8_000, available_bytes: 2_000, swap_total_bytes: 1_000, swap_free_bytes: 750 };
        assert_eq!(m.used_percent(), 75.0);
        assert_eq!(m.swap_used_percent(), Some(25.0));
        assert_eq!(MemoryReport { swap_total_bytes: 0, ..m }.swap_used_percent(), None, "no swap is not 0 % used");
        assert_eq!(percent(1, 0), 0.0);
    }
}
