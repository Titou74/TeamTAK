//! The agent itself: one report, and four actions. That list is closed. No request can
//! name a command, a path or a container: a restart names one of this stack's services,
//! and everything else takes no target at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use time::OffsetDateTime;

use crate::docker::{self, Docker, DockerError};
use crate::host::{self, HostPaths};
use crate::logs;
use crate::parse::{self, CpuTimes};
use crate::report::*;

/// How long a reading of the package state is reused. It takes a second or two of CPU and
/// changes at most a few times a day.
const UPDATES_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Shared with the platform; every request must carry it.
    pub token: String,
    /// Name of the Compose project: the only containers the agent looks at or touches.
    pub project: String,
    /// The repository, mounted read-only (deploy/data/tak and deploy/backups excepted).
    pub repo_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub host: HostPaths,
}

impl AgentConfig {
    pub fn deploy_dir(&self) -> PathBuf {
        self.repo_dir.join("deploy")
    }

    fn tak_logs_dir(&self) -> PathBuf {
        self.deploy_dir().join("data/tak/logs")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// The service is not one the agent may restart.
    NotAllowed,
    /// The stack has no container for it.
    NotFound,
    /// Another action is running.
    Busy,
    Failed(String),
}

impl From<DockerError> for ActionError {
    fn from(e: DockerError) -> Self {
        ActionError::Failed(e.to_string())
    }
}

pub struct Agent {
    pub config: AgentConfig,
    docker: Arc<dyn Docker>,
    cpu: Mutex<Option<CpuTimes>>,
    updates: Mutex<Option<(Instant, UpdatesReport)>>,
    updates_refreshing: AtomicBool,
    /// Restart, backup and certificate hand-over never run two at a time.
    action: tokio::sync::Mutex<()>,
    backup_running: AtomicBool,
}

fn stamp(at: OffsetDateTime) -> String {
    format!("{:04}{:02}{:02}T{:02}{:02}{:02}Z", at.year(), at.month() as u8, at.day(), at.hour(), at.minute(), at.second())
}

fn rfc3339(at: SystemTime) -> Option<String> {
    OffsetDateTime::from(at).format(&time::format_description::well_known::Rfc3339).ok()
}

/// Last lines of a script's output, for the error shown to the administrator.
fn tail(output: &str, lines: usize) -> String {
    let all: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// `key=value` pairs of the line a script ends with (`BACKUP_RESULT file=… size=…`).
fn result_line<'a>(output: &'a str, marker: &str) -> Option<Vec<(&'a str, &'a str)>> {
    let line = output.lines().rev().find_map(|l| l.strip_prefix(marker))?;
    Some(line.split_whitespace().filter_map(|pair| pair.split_once('=')).collect())
}

impl Agent {
    pub fn new(config: AgentConfig, docker: Arc<dyn Docker>) -> Self {
        Self {
            config,
            docker,
            cpu: Mutex::new(None),
            updates: Mutex::new(None),
            updates_refreshing: AtomicBool::new(false),
            action: tokio::sync::Mutex::new(()),
            backup_running: AtomicBool::new(false),
        }
    }

    fn env_text(&self) -> String {
        std::fs::read_to_string(self.config.deploy_dir().join(".env")).unwrap_or_default()
    }

    // --------------------------------------------------------------------------- report

    /// Reads the package state now and remembers it.
    pub async fn refresh_updates(&self) {
        let os = std::fs::read_to_string(self.config.host.root.join("etc/os-release")).map(|t| parse::os_release(&t)).unwrap_or_default();
        let report = match host::distribution_for(&os) {
            Some(distribution) => distribution.updates(&self.config.host.root).await,
            None => UpdatesReport::default(),
        };
        *self.updates.lock().unwrap() = Some((Instant::now(), report));
    }

    /// The package state last read; a new reading is started in the background when it is
    /// old, so a report never waits for the package manager.
    fn updates(self: &Arc<Self>) -> UpdatesReport {
        let cached = self.updates.lock().unwrap().clone();
        let stale = cached.as_ref().is_none_or(|(at, _)| at.elapsed() > UPDATES_TTL);
        if stale && !self.updates_refreshing.swap(true, Ordering::SeqCst) {
            let agent = self.clone();
            tokio::spawn(async move {
                agent.refresh_updates().await;
                agent.updates_refreshing.store(false, Ordering::SeqCst);
            });
        }
        cached.map(|(_, report)| report).unwrap_or_else(|| UpdatesReport { error: Some("first check in progress".into()), ..Default::default() })
    }

    fn backups(&self) -> BackupsReport {
        let mut report = BackupsReport { running: self.backup_running.load(Ordering::SeqCst), ..Default::default() };
        let entries = match std::fs::read_dir(&self.config.backup_dir) {
            Ok(entries) => entries,
            Err(e) => {
                report.error = Some(format!("backup folder: {e}"));
                return report;
            }
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(encrypted) = parse::backup_file(&name) else { continue };
            let Ok(metadata) = entry.metadata() else { continue };
            if !metadata.is_file() {
                continue;
            }
            report.total_bytes += metadata.len();
            report.files.push(BackupFile { name, size_bytes: metadata.len(), modified_at: metadata.modified().ok().and_then(rfc3339), encrypted });
        }
        // The name carries the date: newest first.
        report.files.sort_by(|a, b| b.name.cmp(&a.name));
        report
    }

    fn letsencrypt(&self, env: &str) -> LetsEncryptReport {
        let enabled = parse::env_value(env, "TAK_ENROLL_LETSENCRYPT").as_deref() == Some("true");
        let host = parse::env_value(env, "PUBLIC_HOST").unwrap_or_default();
        let data = self.config.deploy_dir().join("data");
        let certificate = find_caddy_certificate(&data.join("caddy/data/caddy/certificates"), &host);
        let keystore = data.join("tak/certs/letsencrypt.jks");
        let newer_than_certificate = |jks: &Path, crt: &Path| {
            let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
            matches!((modified(jks), modified(crt)), (Some(j), Some(c)) if j >= c)
        };
        LetsEncryptReport {
            enabled,
            certificate_found: certificate.is_some(),
            handed_over: certificate.as_deref().is_some_and(|crt| newer_than_certificate(&keystore, crt)),
        }
    }

    async fn tak(&self, env: &str, containers: &[ContainerReport]) -> TakReport {
        let mut report = TakReport {
            installed_version: parse::env_value(env, "TAK_VERSION").unwrap_or_default(),
            logs_bytes: logs::folder_size(&self.config.tak_logs_dir()),
            ..Default::default()
        };
        if let Ok(entries) = std::fs::read_dir(&self.config.repo_dir) {
            report.zip_versions = entries.flatten().filter_map(|e| parse::zip_version(&e.file_name().to_string_lossy())).collect();
            report.zip_versions.sort();
        }
        match containers.iter().find(|c| c.service == "takserver" && c.state == "running") {
            None => report.processes_error = Some("the takserver container is not running".into()),
            Some(container) => match self.docker.run(&["top", &container.name, "-eo", "pid,args"], None, Duration::from_secs(10)).await {
                Ok(output) => report.processes = parse::tak_processes(&output),
                Err(e) => report.processes_error = Some(e.to_string()),
            },
        }
        report
    }

    pub async fn report(self: &Arc<Self>) -> Report {
        let env = self.env_text();
        let mode = parse::env_value(&env, "MODE").unwrap_or_default();
        let deploy = self.config.deploy_dir();
        let places = [
            ("root", self.config.host.root.join("etc")),
            ("docker", self.config.host.root.join("var/lib/docker")),
            ("data", deploy.join("data")),
            ("backups", self.config.backup_dir.clone()),
        ];
        let previous = *self.cpu.lock().unwrap();
        let (mut host, cpu_times) = host::collect(&self.config.host, &places, previous).await;
        *self.cpu.lock().unwrap() = cpu_times;
        host.updates = self.updates();

        let mut expected: Vec<String> = CORE_SERVICES.iter().map(|s| s.to_string()).collect();
        if mode == "local" {
            expected.extend(LOCAL_SERVICES.iter().map(|s| s.to_string()));
        }
        let containers = match docker::container_reports(self.docker.as_ref(), &self.config.project).await {
            Ok(containers) => ContainersReport { expected, containers, error: None },
            Err(e) => ContainersReport { expected, containers: Vec::new(), error: Some(e.to_string()) },
        };
        let tak = self.tak(&env, &containers.containers).await;
        Report {
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: OffsetDateTime::now_utc(),
            mode,
            host,
            containers,
            tak,
            backups: self.backups(),
            letsencrypt: self.letsencrypt(&env),
        }
    }

    // --------------------------------------------------------------------------- actions

    /// Restarts one service of this stack.
    pub async fn restart(&self, service: &str) -> Result<RestartResult, ActionError> {
        // The name is compared with a fixed list before anything is asked of Docker, and
        // Docker is then given the container's identifier, never the name received.
        if !RESTARTABLE_SERVICES.contains(&service) {
            return Err(ActionError::NotAllowed);
        }
        let _guard = self.action.try_lock().map_err(|_| ActionError::Busy)?;
        let id = docker::service_container(self.docker.as_ref(), &self.config.project, service).await?.ok_or(ActionError::NotFound)?;
        tracing::info!(service, container = %id, "restarting a service");
        self.docker.run(&["restart", "--time", "30", &id], None, Duration::from_secs(180)).await?;
        Ok(RestartResult { service: service.to_string(), container: id })
    }

    async fn script(&self, name: &str, envs: &[(&str, String)], timeout: Duration) -> Result<(bool, String), ActionError> {
        let deploy = self.config.deploy_dir();
        let path = deploy.join("scripts").join(name);
        if !path.is_file() {
            return Err(ActionError::Failed(format!("{} is missing", path.display())));
        }
        let mut command = tokio::process::Command::new("bash");
        command
            .arg(&path)
            .env("TM_DEPLOY_DIR", &deploy)
            .env("TM_COMPOSE_PROJECT", &self.config.project)
            .envs(envs.iter().map(|(k, v)| (*k, v.as_str())))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().map_err(|e| ActionError::Failed(format!("starting {name}: {e}")))?;
        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| ActionError::Failed(format!("{name} did not finish in {} minutes", timeout.as_secs() / 60)))?
            .map_err(|e| ActionError::Failed(e.to_string()))?;
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        Ok((output.status.success(), text))
    }

    /// Writes a backup archive with deploy/scripts/backup.sh.
    pub async fn backup(&self, request: BackupRequest) -> Result<BackupResult, ActionError> {
        let _guard = self.action.try_lock().map_err(|_| ActionError::Busy)?;
        self.backup_running.store(true, Ordering::SeqCst);
        let started = Instant::now();
        let passphrase = request.passphrase.unwrap_or_default();
        let envs = [
            ("BACKUP_DIR", self.config.backup_dir.display().to_string()),
            ("BACKUP_KEEP", request.keep.unwrap_or(14).clamp(1, 1000).to_string()),
            // In the environment of the script, never on a command line.
            ("BACKUP_PASSPHRASE", passphrase.clone()),
        ];
        let outcome = self.script("backup.sh", &envs, Duration::from_secs(30 * 60)).await;
        self.backup_running.store(false, Ordering::SeqCst);
        let (ok, output) = outcome?;
        let duration_seconds = started.elapsed().as_secs();
        let fields = result_line(&output, "BACKUP_RESULT ");
        let field = |name: &str| fields.as_ref().and_then(|f| f.iter().find(|(k, _)| *k == name)).map(|(_, v)| *v);
        match (ok, field("file")) {
            (true, Some(file)) if parse::backup_file(file).is_some() => {
                tracing::info!(file, duration_seconds, "backup written");
                Ok(BackupResult {
                    ok: true,
                    file: Some(file.to_string()),
                    size_bytes: field("size").and_then(|s| s.parse().ok()),
                    encrypted: field("encrypted") == Some("1"),
                    duration_seconds,
                    pruned: field("pruned").and_then(|s| s.parse().ok()).unwrap_or(0),
                    error: None,
                })
            }
            _ => {
                let mut error = tail(&output, 8);
                if !passphrase.is_empty() {
                    error = error.replace(&passphrase, "***");
                }
                tracing::error!(error = %error, "backup failed");
                Ok(BackupResult { ok: false, duration_seconds, encrypted: !passphrase.is_empty(), error: Some(error), ..Default::default() })
            }
        }
    }

    /// Hands Caddy's Let's Encrypt certificate to TAK Server's enrollment port, with
    /// deploy/scripts/sync-letsencrypt.sh. TAK Server is restarted only when the certificate
    /// changed.
    pub async fn letsencrypt_sync(&self) -> Result<LetsEncryptResult, ActionError> {
        let _guard = self.action.try_lock().map_err(|_| ActionError::Busy)?;
        let (ok, output) = self.script("sync-letsencrypt.sh", &[], Duration::from_secs(15 * 60)).await?;
        let outcome = result_line(&output, "LETSENCRYPT_RESULT ").and_then(|f| f.iter().find(|(k, _)| *k == "outcome").map(|(_, v)| v.to_string()));
        match (ok, outcome) {
            (true, Some(outcome)) if outcome == "unchanged" || outcome == "updated" => Ok(LetsEncryptResult { outcome, error: None }),
            _ => Ok(LetsEncryptResult { outcome: "failed".into(), error: Some(tail(&output, 8)) }),
        }
    }

    /// Deletes old rolled logs of TAK Server and cuts the live ones that grew too large.
    pub async fn rotate_logs(&self, request: RotateRequest) -> Result<RotateResult, ActionError> {
        let dir = self.config.tak_logs_dir();
        let keep_days = request.keep_days.unwrap_or(logs::DEFAULT_KEEP_DAYS).clamp(1, 3650);
        let stamp = stamp(OffsetDateTime::now_utc());
        tokio::task::spawn_blocking(move || logs::rotate(&dir, keep_days, logs::LIVE_LIMIT_BYTES, &stamp))
            .await
            .map_err(|e| ActionError::Failed(e.to_string()))?
            .map_err(|e| ActionError::Failed(format!("TAK Server logs: {e}")))
    }
}

/// Caddy stores a certificate as `<certificates>/<issuer directory>/<host>/<host>.crt`.
fn find_caddy_certificate(certificates: &Path, host: &str) -> Option<PathBuf> {
    // The host comes from deploy/.env; it must still be a plain name to be used in a path.
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
        return None;
    }
    std::fs::read_dir(certificates)
        .ok()?
        .flatten()
        .map(|issuer| issuer.path().join(host).join(format!("{host}.crt")))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::docker::fake::*;

    pub fn test_agent(docker: Arc<FakeDocker>, tag: &str) -> (Arc<Agent>, PathBuf) {
        let root = std::env::temp_dir().join(format!("tm-agent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/deploy/data/tak/logs")).unwrap();
        std::fs::create_dir_all(root.join("repo/deploy/backups")).unwrap();
        std::fs::create_dir_all(root.join("host/etc")).unwrap();
        std::fs::write(root.join("repo/deploy/.env"), "MODE=local\nPUBLIC_HOST=localhost\nTAK_VERSION=5.8-RELEASE-103\nAPP_SECRET=not-in-any-report\n").unwrap();
        let config = AgentConfig {
            token: "t".repeat(40),
            project: "teamtak".into(),
            repo_dir: root.join("repo"),
            backup_dir: root.join("repo/deploy/backups"),
            host: HostPaths { root: root.join("host"), proc: PathBuf::from("/proc") },
        };
        (Arc::new(Agent::new(config, docker)), root)
    }

    #[tokio::test]
    async fn a_restart_is_refused_for_anything_but_a_service_of_this_stack() {
        let docker = Arc::new(FakeDocker { leaky_filter: true, ..Default::default() });
        let (agent, root) = test_agent(docker.clone(), "refuse");
        for target in [
            "mapping-db-1",          // a container of another project
            "silicate-db",
            "agent",                 // itself
            "teamtak-caddy-1",   // a container name instead of a service
            FOREIGN_ID,              // an identifier
            "caddy; docker ps",
            "../caddy",
            "--all",
            "",
        ] {
            assert_eq!(agent.restart(target).await, Err(ActionError::NotAllowed), "{target:?}");
        }
        assert!(docker.calls().is_empty(), "Docker was not even asked: {:?}", docker.calls());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_restart_acts_on_the_container_of_this_project_only() {
        let docker = Arc::new(FakeDocker { leaky_filter: true, ..Default::default() });
        let (agent, root) = test_agent(docker.clone(), "restart");
        let done = agent.restart("caddy").await.unwrap();
        assert_eq!(done.container, CADDY_ID);
        assert_eq!(docker.actions(), vec![vec!["restart".to_string(), "--time".into(), "30".into(), CADDY_ID.to_string()]]);
        // An allowed service this stack does not run (the mail catcher in production).
        assert_eq!(agent.restart("mailpit").await, Err(ActionError::NotFound));
        assert_eq!(docker.actions().len(), 1, "nothing else was restarted");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn the_report_describes_this_stack_and_carries_no_secret() {
        let docker = Arc::new(FakeDocker { leaky_filter: true, ..Default::default() });
        let (agent, root) = test_agent(docker.clone(), "report");
        std::fs::write(root.join("repo/takserver-docker-hardened-5.9-RELEASE-7.zip"), "").unwrap();
        std::fs::write(root.join("repo/deploy/backups/teamtak-backup-20261119T031500Z.tar.gz.enc"), "12345").unwrap();
        std::fs::write(root.join("repo/deploy/backups/teamtak-backup-20261118T031500Z.tar.gz"), "123").unwrap();
        std::fs::write(root.join("repo/deploy/backups/unrelated.txt"), "x").unwrap();

        let report = agent.report().await;
        assert_eq!(report.mode, "local");
        let services: Vec<&str> = report.containers.containers.iter().map(|c| c.service.as_str()).collect();
        assert_eq!(services, vec!["caddy", "takserver"]);
        assert!(report.containers.containers.iter().all(|c| c.name.starts_with("teamtak-")), "nothing of the other project");
        assert!(report.containers.expected.contains(&"mailpit".to_string()), "local mode expects the mail catcher");
        assert_eq!(report.tak.installed_version, "5.8-RELEASE-103");
        assert_eq!(report.tak.zip_versions, vec!["5.9-RELEASE-7".to_string()]);
        assert!(report.tak.processes.iter().any(|p| p.name == "api" && p.running));
        let names: Vec<&str> = report.backups.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["teamtak-backup-20261119T031500Z.tar.gz.enc", "teamtak-backup-20261118T031500Z.tar.gz"],
            "newest first, and only the script's archives"
        );
        assert_eq!((report.backups.files[0].encrypted, report.backups.total_bytes), (true, 8));
        assert!(!report.letsencrypt.enabled);
        // Unknown distribution (no os-release in the fake host): nothing is claimed.
        assert_eq!((report.host.updates.manager.clone(), report.host.updates.pending), (None, None));

        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("not-in-any-report"), "the report must not leak deploy/.env");
        assert_eq!(serde_json::from_str::<Report>(&json).unwrap(), report, "the server reads back what the agent wrote");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_docker_that_does_not_answer_is_reported_as_such() {
        let docker = Arc::new(FakeDocker { down: true, ..Default::default() });
        let (agent, root) = test_agent(docker, "down");
        let report = agent.report().await;
        assert!(report.containers.error.is_some() && report.containers.containers.is_empty());
        assert!(report.tak.processes.is_empty() && report.tak.processes_error.is_some());
        assert!(matches!(agent.restart("caddy").await, Err(ActionError::Failed(_))));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn script_results_are_read_from_their_last_line() {
        let output = "dumping…\nBACKUP_RESULT file=old size=1\nmore\nBACKUP_RESULT file=teamtak-backup-20261119T031500Z.tar.gz size=4096 encrypted=0 pruned=2\n";
        let fields = result_line(output, "BACKUP_RESULT ").unwrap();
        assert_eq!(fields[0], ("file", "teamtak-backup-20261119T031500Z.tar.gz"));
        assert_eq!(fields[3], ("pruned", "2"));
        assert!(result_line("nothing here", "BACKUP_RESULT ").is_none());
        assert_eq!(tail("a\n\nb\nc\n", 2), "b\nc");
    }

    #[test]
    fn the_caddy_certificate_is_looked_up_for_a_plain_host_name_only() {
        let root = std::env::temp_dir().join(format!("tm-agent-le-{}", std::process::id()));
        let dir = root.join("acme-v02.api.letsencrypt.org-directory/tak.example.org");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tak.example.org.crt"), "x").unwrap();
        assert_eq!(find_caddy_certificate(&root, "tak.example.org"), Some(dir.join("tak.example.org.crt")));
        assert_eq!(find_caddy_certificate(&root, "other.example.org"), None);
        assert_eq!(find_caddy_certificate(&root, "../tak.example.org"), None);
        assert_eq!(find_caddy_certificate(&root, ""), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn archive_names_carry_a_sortable_utc_date() {
        let at = time::macros::datetime!(2026-11-09 03:05:07 UTC);
        assert_eq!(stamp(at), "20261109T030507Z");
        assert_eq!(parse::backup_file(&format!("{}{}.tar.gz", parse::BACKUP_PREFIX, stamp(at))), Some(false));
    }
}
