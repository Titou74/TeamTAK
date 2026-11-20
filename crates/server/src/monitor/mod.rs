//! Monitoring: one background loop that looks at everything, keeps the result for the
//! dashboard, raises and resolves the alerts, and runs the daily jobs.
//!
//! Three rules shape it:
//!
//! - **It is never on a user's path.** The loop probes; requests read what the loop last
//!   saw. A slow or dead probe delays the next look, not a page.
//! - **Every probe has a timeout**, and a part that could not be read is shown as unknown
//!   and leaves its alerts as they are.
//! - **The platform works without the agent.** What the platform sees by itself (TAK
//!   Server's ports and API, the directory, certificates, the reconciliation, e-mails) is
//!   still watched; the rest is shown as unavailable, and the dashboard says why.

pub mod agent;
pub mod alerting;
pub mod api;
pub mod devices;
pub mod images;
pub mod maintenance;
pub mod probes;
pub mod release;
pub mod snapshot;

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
use sqlx::PgPool;
use teamtak_agent::report::{Report, RESTARTABLE_SERVICES};
use teamtak_core::alerts::{
    AgentState, BackupObservation, Boundary, CertificateObservation, MailObservation, Observations, OverdueEvent, ReconcileObservation,
    ServiceObservation, Streaks,
};
use teamtak_core::certs::{days_left, parse_pem, CertificateInfo};
use teamtak_core::secret::SecretBox;
use teamtak_core::tak::{Subscription, TakServer};
use teamtak_core::versions::parse_tak_version;
use time::OffsetDateTime;
use tokio::sync::Notify;

use crate::config::Config;
use crate::mail::Mailer;
use crate::settings::{self, Stored};

use self::agent::{AgentCallError, AgentClient};
use self::probes::{host_port, tcp_open, tls_probe, TlsProbe};
use self::snapshot::*;

/// A history sample is kept this often, for the machine and for each service.
const SAMPLE_EVERY: time::Duration = time::Duration::minutes(1);
/// Two looks a minute apart are not always 60 seconds apart: one that comes this much early
/// is still sampled, so that the default look of every minute gives one sample a minute.
const SAMPLE_SLACK: time::Duration = time::Duration::seconds(15);
/// Restarts are counted over this window to tell a loop from a single restart.
const RESTART_WINDOW: time::Duration = time::Duration::minutes(15);

/// The addresses and files the monitoring looks at, copied from the configuration.
#[derive(Debug, Clone)]
struct Targets {
    interval: Duration,
    reconcile_interval: Duration,
    /// Host of TAK Server inside the network, and its API port.
    tak_host: String,
    /// Ports of the machine TAK clients connect to (`TAK_CLIENT_PORTS`), the main one first.
    client_ports: Vec<u16>,
    tak_api_port: u16,
    /// Host and port of the enrollment connector as the platform reaches it.
    enroll_host: String,
    enroll_port: u16,
    ldap: Option<(String, u16)>,
    ca_file: std::path::PathBuf,
    api_cert_file: std::path::PathBuf,
    /// The certificate Caddy presents for TAK Server's administration pages; `None` when
    /// those pages are turned off, so an unused certificate raises no alert.
    admin_ui_cert_file: Option<std::path::PathBuf>,
    /// Where the reverse proxy answers in TLS, and the name it serves; `None` in local mode.
    web: Option<(String, u16, String)>,
    smtp_configured: bool,
    public_url: String,
    /// GitHub repository TeamTAK's releases are looked for in; `None`: not looked for.
    release_repository: Option<String>,
}

/// TAK Server's single TLS input for its clients, inside the network: every port of
/// `TAK_CLIENT_PORTS` is published to it.
const TAK_INPUT_PORT: u16 = 8089;

/// Ports of the machine Docker publishes to TAK Server's input, from the agent's report.
/// `None` when the report cannot tell: Docker did not answer, no TAK Server container, or
/// an agent too old to report them.
fn published_to_input(report: &Report) -> Option<Vec<u16>> {
    if report.containers.error.is_some() {
        return None;
    }
    let takserver = report.containers.containers.iter().find(|c| c.service == "takserver")?;
    let published = takserver.published_ports.as_ref()?;
    Some(published.iter().filter(|p| p.container_port == TAK_INPUT_PORT && p.protocol == "tcp").map(|p| p.host_port).collect())
}

/// One view per port of TAK_CLIENT_PORTS. A port answers when TAK Server's input does and
/// Docker publishes it there; when what Docker publishes is unknown, the input alone decides.
fn client_port_views(client_ports: &[u16], input_open: bool, published: Option<&[u16]>) -> Vec<PortView> {
    client_ports
        .iter()
        .map(|&port| {
            let published = published.map(|list| list.contains(&port));
            PortView { port, role: "clients", open: input_open && published != Some(false), published }
        })
        .collect()
}

pub struct Monitor {
    db: PgPool,
    targets: Targets,
    tak: Arc<dyn TakServer>,
    agent: Option<AgentClient>,
    mailer: Arc<Mailer>,
    secrets: SecretBox,
    /// For the registries, GitHub and the webhook; `None` if it could not be built.
    http: Option<reqwest::Client>,
    started_at: OffsetDateTime,
    latest: RwLock<Option<Arc<Snapshot>>>,
    pub devices: devices::DeviceBoard,
    streaks: Mutex<Streaks>,
    restarts: Mutex<RestartHistory>,
    /// Certificates the servers presented last time they answered.
    presented: Mutex<BTreeMap<&'static str, CertificateInfo>>,
    /// Ports of the machine Docker publishes to TAK Server's input, last time the agent told.
    published: Mutex<Option<Vec<u16>>>,
    last_sample: Mutex<Option<OffsetDateTime>>,
    /// One look at a time.
    cycle: tokio::sync::Mutex<()>,
    kick: Notify,
    daily_running: AtomicBool,
    images_running: AtomicBool,
    release_running: AtomicBool,
    /// When the webhook was last posted to, and the outcome.
    webhook_last: Mutex<Option<(OffsetDateTime, Result<u16, String>)>>,
}

/// Restart counters of each service over the recent past.
#[derive(Debug, Default)]
pub struct RestartHistory {
    samples: BTreeMap<String, VecDeque<(OffsetDateTime, u32)>>,
}

impl RestartHistory {
    /// Records the restart counter Docker gives for a service and returns how many
    /// restarts happened within the window.
    pub fn record(&mut self, service: &str, count: u32, now: OffsetDateTime) -> u32 {
        let samples = self.samples.entry(service.to_string()).or_default();
        // A counter that went back: the container was created again; start over.
        if samples.back().is_some_and(|(_, last)| count < *last) {
            samples.clear();
        }
        samples.push_back((now, count));
        while samples.front().is_some_and(|(at, _)| now - *at > RESTART_WINDOW) {
            samples.pop_front();
        }
        count - samples.front().map(|(_, first)| *first).unwrap_or(count)
    }

    /// Forgets services that no longer exist.
    pub fn keep_only(&mut self, services: &[&str]) {
        self.samples.retain(|name, _| services.contains(&name.as_str()));
    }
}

/// What one look produced.
struct Collected {
    snapshot: Snapshot,
    observations: Observations,
    subscriptions: Option<Vec<Subscription>>,
}

impl Monitor {
    pub fn new(config: &Config, db: PgPool, tak: Arc<dyn TakServer>, mailer: Arc<Mailer>, secrets: SecretBox) -> Self {
        let (tak_host, tak_api_port) = host_port(&config.tak.api_url, 8443).unwrap_or(("takserver".into(), 8443));
        let (enroll_host, enroll_port) = host_port(&config.tak.enroll_url, 8446).unwrap_or((tak_host.clone(), 8446));
        // The proxy's certificate is only worth reading when the UI is served over HTTPS.
        let web = config.public_url.strip_prefix("https://").and_then(|_| {
            let (name, _) = host_port(&config.public_url, 443)?;
            let (host, port) = host_port(config.monitor.web_tls_probe.as_deref()?, 443)?;
            Some((host, port, name))
        });
        let agent = config.monitor.agent_url.as_deref().and_then(|url| match AgentClient::new(url, &config.monitor.agent_token) {
            Ok(client) => Some(client),
            Err(e) => {
                tracing::error!(error = %e, "host agent client");
                None
            }
        });
        if agent.is_none() {
            tracing::warn!("no host agent configured (AGENT_URL): the machine and the services are not monitored");
        }
        Self {
            db,
            targets: Targets {
                interval: config.monitor.interval,
                reconcile_interval: config.reconcile_interval,
                tak_host,
                client_ports: config.tak.client_ports.clone(),
                tak_api_port,
                enroll_host,
                enroll_port,
                ldap: host_port(&config.ldap.url, 389),
                ca_file: config.tak.ca_file.clone(),
                api_cert_file: config.tak.api_cert_file.clone(),
                admin_ui_cert_file: config.tak_admin_url.as_ref().and(config.tak.admin_ui_cert_file.clone()),
                web,
                smtp_configured: !config.smtp.host.is_empty(),
                public_url: config.public_url.clone(),
                release_repository: config.monitor.release_repository.clone(),
            },
            tak,
            agent,
            mailer,
            secrets,
            http: teamtak_core::public_http_client(Duration::from_secs(10)).map_err(|e| tracing::error!(error = %e, "HTTPS client")).ok(),
            started_at: OffsetDateTime::now_utc(),
            latest: RwLock::new(None),
            devices: devices::DeviceBoard::default(),
            streaks: Mutex::new(Streaks::default()),
            restarts: Mutex::new(RestartHistory::default()),
            presented: Mutex::new(BTreeMap::new()),
            published: Mutex::new(None),
            last_sample: Mutex::new(None),
            cycle: tokio::sync::Mutex::new(()),
            kick: Notify::new(),
            daily_running: AtomicBool::new(false),
            images_running: AtomicBool::new(false),
            release_running: AtomicBool::new(false),
            webhook_last: Mutex::new(None),
        }
    }

    /// The latest look; `None` until the first one is done.
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        self.latest.read().unwrap().clone()
    }

    pub fn agent(&self) -> Option<&AgentClient> {
        self.agent.as_ref()
    }

    /// Asks the loop to look again without waiting for its next turn.
    pub fn refresh_soon(&self) {
        self.kick.notify_one();
    }

    /// Looks now and returns what was seen. Used by an administrator's "refresh".
    pub async fn refresh_now(self: &Arc<Self>) -> Option<Arc<Snapshot>> {
        if let Err(e) = self.run_cycle().await {
            tracing::error!(error = %format!("{e:#}"), "monitoring look failed");
        }
        self.latest()
    }

    // --------------------------------------------------------------------------- collecting

    /// Reads the certificate authority and the platform's own certificate from its files.
    fn file_certificates(&self) -> Vec<(&'static str, CertificateInfo)> {
        let mut found = Vec::new();
        match std::fs::read(&self.targets.ca_file).map_err(|e| e.to_string()).and_then(|pem| parse_pem(&pem)) {
            Ok(authorities) => {
                for authority in authorities.into_iter().filter(|c| c.is_ca) {
                    let id = if authority.self_signed() { "root_ca" } else { "intermediate_ca" };
                    if !found.iter().any(|(known, _)| *known == id) {
                        found.push((id, authority));
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "reading the certificate authority file"),
        }
        match std::fs::read(&self.targets.api_cert_file).map_err(|e| e.to_string()).and_then(|pem| parse_pem(&pem)) {
            Ok(certificates) => found.extend(certificates.into_iter().find(|c| !c.is_ca).map(|c| ("platform_api", c))),
            Err(e) => tracing::warn!(error = %e, "reading the platform's API certificate"),
        }
        if let Some(file) = &self.targets.admin_ui_cert_file {
            match std::fs::read(file).map_err(|e| e.to_string()).and_then(|pem| parse_pem(&pem)) {
                Ok(certificates) => found.extend(certificates.into_iter().find(|c| !c.is_ca).map(|c| ("tak_admin_ui", c))),
                Err(e) => tracing::warn!(error = %e, "reading the certificate of TAK Server's administration pages"),
            }
        }
        found
    }

    /// The certificate a server presents, or the one it presented last time when it does
    /// not answer now.
    fn presented(&self, id: &'static str, probe: &TlsProbe) -> Option<(CertificateInfo, bool)> {
        let mut known = self.presented.lock().unwrap();
        match &probe.certificate {
            Some(certificate) => {
                known.insert(id, certificate.clone());
                Some((certificate.clone(), false))
            }
            None => known.get(id).cloned().map(|c| (c, true)),
        }
    }

    async fn collect(&self, stored: &Stored) -> Result<Collected> {
        let settings = &stored.values;
        let t = &self.targets;
        let quick = |seconds| Duration::from_secs(seconds);

        let agent_call = async {
            match &self.agent {
                None => None,
                Some(agent) => Some(agent.report().await),
            }
        };
        let version_call = async { tokio::time::timeout(quick(8), self.tak.version()).await.ok().and_then(Result::ok) };
        let sessions_call = async { tokio::time::timeout(quick(10), self.tak.subscriptions()).await.ok().and_then(Result::ok) };
        let ldap_call = async {
            match &t.ldap {
                Some((host, port)) => Some(tcp_open(host, *port).await),
                None => None,
            }
        };
        let web_call = async {
            match &t.web {
                Some((host, port, name)) => Some(tls_probe(host, *port, name).await),
                None => None,
            }
        };
        let (agent_answer, input_open, api, enroll, version, subscriptions, ldap_open, web) = tokio::join!(
            agent_call,
            // The clients' input is only asked whether it accepts connections. A TLS exchange
            // without a client certificate would be logged by TAK Server as an error, at
            // every look; the same certificate is read on the API port, which stays quiet.
            tcp_open(&t.tak_host, TAK_INPUT_PORT),
            tls_probe(&t.tak_host, t.tak_api_port, &t.tak_host),
            tls_probe(&t.enroll_host, t.enroll_port, &t.enroll_host),
            version_call,
            sessions_call,
            ldap_call,
            web_call,
        );
        let now = OffsetDateTime::now_utc();

        let (agent_view, report): (AgentView, Option<Report>) = match agent_answer {
            None => (AgentView { status: "disabled", error: None, version: None }, None),
            Some(Ok(report)) => (AgentView { status: "ok", error: None, version: Some(report.agent_version.clone()) }, Some(report)),
            Some(Err(e)) => {
                let error = match &e {
                    AgentCallError::Unreachable(reason) => reason.clone(),
                    AgentCallError::Refused { code, .. } => code.clone(),
                };
                (AgentView { status: "unreachable", error: Some(error), version: None }, None)
            }
        };
        let mut obs = Observations::empty(now);
        obs.agent = match agent_view.status {
            "ok" => AgentState::Ok,
            "unreachable" => AgentState::Unreachable,
            _ => AgentState::Disabled,
        };

        // ----- the machine
        if let Some(report) = &report {
            let host = &report.host;
            obs.disks = Some(host.disks.iter().map(|d| (d.name.clone(), d.used_percent)).collect());
            obs.memory_used_percent = host.memory.map(|m| m.used_percent());
            obs.clock_synchronized = host.clock.synchronized;
            obs.security_updates = host.updates.security;
            obs.reboot_required = host.updates.reboot_required;
        }

        // ----- the services
        let image_checks = images::load_checks(&self.db).await.unwrap_or_default();
        let mut services = ServicesView::default();
        if let Some(report) = &report {
            services.error = report.containers.error.clone();
            let mut restarts = self.restarts.lock().unwrap();
            let expected: Vec<&str> = report.containers.expected.iter().map(String::as_str).collect();
            restarts.keep_only(&expected);
            for name in &report.containers.expected {
                let container = report.containers.containers.iter().find(|c| &c.service == name);
                let view = match container {
                    Some(c) => ServiceView {
                        service: name.clone(),
                        present: true,
                        state: Some(c.state.clone()),
                        health: c.health.clone(),
                        restart_count: c.restart_count,
                        recent_restarts: restarts.record(name, c.restart_count, now),
                        started_at: c.started_at.clone(),
                        image: c.image.clone(),
                        image_created: c.image_created.clone(),
                        restartable: RESTARTABLE_SERVICES.contains(&name.as_str()),
                        update: images::status(&c.image, &c.repo_digests, &image_checks),
                        cpu_percent: c.cpu_percent,
                        memory_bytes: c.memory_bytes,
                        disk_bytes: c.disk_bytes,
                    },
                    None => ServiceView { service: name.clone(), restartable: false, ..Default::default() },
                };
                services.services.push(view);
            }
            // Without Docker's answer nothing is known about the services: not "all missing".
            if report.containers.error.is_none() {
                obs.services = Some(
                    services
                        .services
                        .iter()
                        .map(|s| ServiceObservation { name: s.service.clone(), state: s.state.clone(), health: s.health.clone(), recent_restarts: s.recent_restarts })
                        .collect(),
                );
                if report.tak.processes_error.is_none() && !report.tak.processes.is_empty() {
                    obs.tak_processes = Some(report.tak.processes.iter().map(|p| (p.name.clone(), p.running)).collect());
                }
            }
        }

        // ----- TAK Server
        // Each port of TAK_CLIENT_PORTS answers when TAK Server's input does and Docker
        // publishes that port to it. Connecting to the ports of the machine from a container
        // would depend on the address they are bound to and on the machine's firewall, which
        // the phones' connections do not go through: the platform would raise false alarms.
        let published = {
            let mut last = self.published.lock().unwrap();
            if let Some(seen) = report.as_ref().and_then(published_to_input) {
                *last = Some(seen);
            }
            last.clone()
        };
        let mut ports = client_port_views(&t.client_ports, input_open, published.as_deref());
        ports.push(PortView { port: t.tak_api_port, role: "api", open: api.open, published: None });
        ports.push(PortView { port: t.enroll_port, role: "enrollment", open: enroll.open, published: None });
        let mut failures: Vec<String> = Vec::new();
        for port in ports.iter().filter(|p| !p.open).map(|p| p.port.to_string()) {
            // A client port can be 8443 too, TAK Server's API port inside the network.
            if !failures.contains(&port) {
                failures.push(port);
            }
        }
        if version.is_none() {
            failures.push("api".to_string());
        }
        obs.tak_failures = Some(failures);
        let installed_version = report.as_ref().map(|r| r.tak.installed_version.clone()).unwrap_or_default();
        // The version to compare with: what runs, else what was installed.
        let current = version.as_deref().and_then(parse_tak_version).or_else(|| parse_tak_version(&installed_version));
        let zip_versions = report.as_ref().map(|r| r.tak.zip_versions.clone()).unwrap_or_default();
        let tak = TakView {
            ports,
            api_ok: version.is_some(),
            version: version.clone(),
            installed_version,
            // Only newer ones: the ZIP of an older version (the one left after an upgrade) is
            // nothing to act on, and install.sh --upgrade-tak refuses it.
            other_zip_versions: zip_versions
                .iter()
                .filter(|z| matches!((parse_tak_version(z), current), (Some(zip), Some(current)) if zip > current))
                .cloned()
                .collect(),
            zip_versions,
            newer_known: matches!((parse_tak_version(&settings.tak_latest_known_version), current), (Some(known), Some(current)) if known > current),
            latest_known_version: settings.tak_latest_known_version.clone(),
            processes: report.as_ref().map(|r| r.tak.processes.clone()).unwrap_or_default(),
            processes_error: report.as_ref().and_then(|r| r.tak.processes_error.clone()),
            logs_bytes: report.as_ref().and_then(|r| r.tak.logs_bytes),
        };

        // ----- certificates
        let warning_days = settings.thresholds.certificate_warning_days;
        let critical_days = settings.thresholds.certificate_critical_days;
        let mut certificates = Vec::new();
        let mut add = |id: &'static str, info: CertificateInfo, from: &'static str, stale: bool| {
            let left = days_left(info.not_after, now);
            let level = if info.not_after <= now { "expired" } else { expiry_level(left, warning_days, critical_days) };
            certificates.push(CertificateView { id, subject: info.subject, issuer: info.issuer, expires_at: info.not_after, days_left: left, level, from, stale });
        };
        for (id, info) in self.file_certificates() {
            add(id, info, "file", false);
        }
        // TAK Server's certificate, as it presents it to the platform (and to the phones: its
        // ports share one certificate, the enrollment port excepted once Let's Encrypt is used).
        let server = self.presented("tak_server", &api);
        if let Some((info, stale)) = &server {
            add("tak_server", info.clone(), "presented", *stale);
        }
        // The enrollment port has its own certificate once Let's Encrypt was handed over.
        if let Some((info, stale)) = self.presented("enrollment", &enroll) {
            if server.as_ref().is_none_or(|(s, _)| s.fingerprint != info.fingerprint) {
                add("enrollment", info, "presented", stale);
            }
        }
        if let Some(probe) = &web {
            if let Some((info, stale)) = self.presented("web", probe) {
                add("web", info, "presented", stale);
            }
        }
        obs.certificates = Some(certificates.iter().map(|c| CertificateObservation { name: c.id.to_string(), expires_at: c.expires_at }).collect());

        // ----- the platform's own state
        let mut platform = self.platform(now, ldap_open).await?;
        let release_check = release::load(&self.db).await.unwrap_or_default();
        let (release_view, release_observation) = release::status(t.release_repository.as_deref(), &release_check);
        platform.release = release_view;
        obs.release = release_observation;
        obs.directory_reachable = ldap_open;
        obs.reconcile = Some(ReconcileObservation { last_ok_at: platform.reconcile.last_ok_at, running_since: Some(self.started_at) });
        obs.overdue_events = Some(
            platform
                .overdue_events
                .iter()
                .map(|e| OverdueEvent {
                    group: e.group.clone(),
                    name: e.name.clone(),
                    boundary: if e.boundary == "end" { Boundary::End } else { Boundary::Start },
                    due_at: e.due_at,
                })
                .collect(),
        );
        obs.mail = Some(MailObservation {
            failures_last_hour: platform.mail.failures_last_hour as u32,
            last_failure_at: platform.mail.last_failure_at,
            last_success_at: platform.mail.last_success_at,
        });

        let client_certificates = self.client_certificates(now, warning_days, critical_days).await?;
        obs.client_certificates = Some((client_certificates.expiring_warning as u32, client_certificates.expiring_critical as u32));

        // ----- backups
        let backups = self.backups(stored, report.as_ref()).await?;
        if report.is_some() {
            let last_failed = backups.runs.first().is_some_and(|run| run.ok == Some(false));
            obs.backups = Some(BackupObservation { enabled: backups.enabled, newest_at: backups.newest_at, since: Some(stored.created_at), last_run_failed: last_failed });
        }

        let devices = DeviceCounts {
            identified: subscriptions.as_ref().map(|s| s.iter().filter(|s| !s.username.is_empty()).count()).unwrap_or(0),
            unidentified: subscriptions.as_ref().map(|s| s.iter().filter(|s| s.username.is_empty()).count()).unwrap_or(0),
        };
        let snapshot = Snapshot {
            collected_at: now,
            interval_seconds: t.interval.as_secs(),
            agent: agent_view,
            mode: report.as_ref().map(|r| r.mode.clone()).unwrap_or_default(),
            letsencrypt: report.as_ref().map(|r| r.letsencrypt.clone()),
            host: report.map(|r| r.host),
            services,
            tak,
            certificates,
            client_certificates,
            platform,
            backups,
            devices,
        };
        Ok(Collected { snapshot, observations: obs, subscriptions })
    }

    async fn platform(&self, now: OffsetDateTime, ldap_open: Option<bool>) -> Result<PlatformView> {
        type StatusRow = (Option<OffsetDateTime>, Option<OffsetDateTime>, Option<OffsetDateTime>, bool, bool, Value);
        let (last_run_at, last_ok_at, desired_at, directory_reachable, tak_reachable, errors): StatusRow = sqlx::query_as(
            "SELECT last_run_at, last_ok_at, desired_at, directory_reachable, tak_reachable, last_errors FROM reconcile_status WHERE id = 1",
        )
        .fetch_one(&self.db)
        .await?;
        let (drift_24h, drift_7d): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE occurred_at > now() - interval '24 hours'), count(*)
               FROM reconcile_events WHERE is_drift AND occurred_at > now() - interval '7 days'",
        )
        .fetch_one(&self.db)
        .await?;
        // A start or an end is applied once a clean pass computed the groups for a later
        // time. Until there has been one clean pass, nothing can be said.
        let events: Vec<(String, String, OffsetDateTime, OffsetDateTime)> = match desired_at {
            None => Vec::new(),
            Some(desired_at) => {
                sqlx::query_as(
                    "SELECT group_name, name, starts_at, ends_at FROM events
                      WHERE cancelled_at IS NULL
                        AND ((starts_at > $1 AND starts_at <= $2) OR (ends_at > $1 AND ends_at <= $2))
                      ORDER BY starts_at LIMIT 50",
                )
                .bind(desired_at)
                .bind(now)
                .fetch_all(&self.db)
                .await?
            }
        };
        let overdue_events = events
            .into_iter()
            .map(|(group, name, starts_at, ends_at)| {
                // Past its end: the end is what matters, whether or not the start was applied.
                let (boundary, due_at) = if ends_at <= now { ("end", ends_at) } else { ("start", starts_at) };
                OverdueEventView { group, name, boundary, due_at }
            })
            .collect();

        let (last_success_at, last_failure_at): (Option<OffsetDateTime>, Option<OffsetDateTime>) =
            sqlx::query_as("SELECT last_mail_ok_at, last_mail_failure_at FROM monitor_state WHERE id = 1").fetch_one(&self.db).await?;
        let (failures_last_hour, failures_24h): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE occurred_at > now() - interval '1 hour'), count(*)
               FROM mail_failures WHERE occurred_at > now() - interval '24 hours'",
        )
        .fetch_one(&self.db)
        .await?;
        let last_error: Option<(String,)> =
            sqlx::query_as("SELECT error FROM mail_failures ORDER BY id DESC LIMIT 1").fetch_optional(&self.db).await?;

        Ok(PlatformView {
            reconcile: ReconcileView {
                last_run_at,
                last_ok_at,
                desired_at,
                directory_reachable,
                tak_reachable,
                errors: errors.as_array().map(|items| items.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                interval_seconds: self.targets.reconcile_interval.as_secs(),
            },
            drift_24h,
            drift_7d,
            overdue_events,
            mail: MailView {
                configured: self.targets.smtp_configured,
                failures_last_hour,
                failures_24h,
                last_failure_at,
                last_success_at,
                // Only while it is the latest word: an old failure followed by a success is history.
                last_error: last_error.map(|(e,)| e).filter(|_| matches!((last_failure_at, last_success_at), (Some(f), s) if s.is_none_or(|s| f > s))),
            },
            directory_open: ldap_open.unwrap_or(false),
            started_at: Some(self.started_at),
            release: ReleaseView::default(),
        })
    }

    async fn client_certificates(&self, now: OffsetDateTime, warning_days: i64, critical_days: i64) -> Result<ClientCertificatesView> {
        // Certificates TAK Server accepts today, of accesses that still exist.
        const ACTIVE: &str = "c.inactive_at IS NULL AND c.revoke_requested_at IS NULL AND c.access_id IS NOT NULL AND c.expires_at IS NOT NULL";
        let (active, expiring_warning, expiring_critical): (i64, i64, i64) = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT count(*),
                    count(*) FILTER (WHERE c.expires_at <= now() + make_interval(days => $1)),
                    count(*) FILTER (WHERE c.expires_at <= now() + make_interval(days => $2))
               FROM tak_certificates c WHERE {ACTIVE}"
        )))
        .bind(warning_days as i32)
        .bind(critical_days as i32)
        .fetch_one(&self.db)
        .await?;
        let rows: Vec<(String, uuid::Uuid, String, String, OffsetDateTime)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT c.username, t.id, t.name, c.source, c.expires_at
               FROM tak_certificates c JOIN tak_accesses a ON a.id = c.access_id JOIN teams t ON t.id = a.team_id
              WHERE {ACTIVE} ORDER BY c.expires_at LIMIT 20"
        )))
        .fetch_all(&self.db)
        .await?;
        Ok(ClientCertificatesView {
            active,
            expiring_warning,
            expiring_critical,
            warning_days,
            critical_days,
            soonest: rows
                .into_iter()
                .map(|(username, team_id, team_name, source, expires_at)| ClientCertificate {
                    username,
                    team_id,
                    team_name,
                    source,
                    expires_at,
                    days_left: days_left(expires_at, now),
                })
                .collect(),
        })
    }

    async fn backups(&self, stored: &Stored, report: Option<&Report>) -> Result<BackupsView> {
        let settings = &stored.values;
        let runs: Vec<BackupRun> = sqlx::query_as(
            "SELECT id, started_at, finished_at, trigger, requested_by, ok, file, size_bytes, encrypted, error
               FROM backup_runs ORDER BY id DESC LIMIT 10",
        )
        .fetch_all(&self.db)
        .await?;
        let (daily_ran_at, daily_report): (Option<OffsetDateTime>, Value) =
            sqlx::query_as("SELECT daily_ran_at, daily_report FROM monitor_state WHERE id = 1").fetch_one(&self.db).await?;
        let files = report.map(|r| r.backups.files.clone()).unwrap_or_default();
        // The newest archive on disk; without agent, the last run that worked.
        let newest_file = files.first().and_then(|f| f.modified_at.as_deref()).and_then(|at| OffsetDateTime::parse(at, &time::format_description::well_known::Rfc3339).ok());
        let newest_run = runs.iter().find(|r| r.ok == Some(true)).and_then(|r| r.finished_at);
        Ok(BackupsView {
            enabled: settings.backup_enabled,
            hour_utc: settings.backup_hour_utc,
            keep: settings.backup_keep,
            passphrase_set: stored.passphrase_enc.is_some(),
            max_age_hours: settings.thresholds.backup_max_age_hours,
            total_bytes: report.map(|r| r.backups.total_bytes).unwrap_or(0),
            running: report.is_some_and(|r| r.backups.running),
            error: report.and_then(|r| r.backups.error.clone()),
            newest_at: if report.is_some() { newest_file } else { newest_run },
            files,
            runs,
            daily_ran_at,
            daily_report,
        })
    }

    // --------------------------------------------------------------------------- the loop

    /// One look: collect, publish, evaluate the alerts, keep a history sample.
    async fn run_cycle(self: &Arc<Self>) -> Result<()> {
        let _one_at_a_time = self.cycle.lock().await;
        let stored = settings::load(&self.db).await?;
        let Collected { snapshot, observations, subscriptions } = self.collect(&stored).await?;
        let now = snapshot.collected_at;

        // Connected devices; an unanswered question keeps the previous list, marked old.
        if let Some(subscriptions) = subscriptions {
            self.devices.update(subscriptions, now);
        }
        self.sample(&snapshot).await;
        *self.latest.write().unwrap() = Some(Arc::new(snapshot));

        alerting::run(self, &stored, &observations).await?;
        alerting::notify_team_leaders(self, &stored).await?;
        maintenance::run_if_due(self, &stored, now);
        images::refresh_if_due(self);
        release::refresh_if_due(self);
        Ok(())
    }

    /// Keeps one history point a minute: the machine's, and one for each service the agent
    /// measured.
    async fn sample(&self, snapshot: &Snapshot) {
        let now = snapshot.collected_at;
        let last = *self.last_sample.lock().unwrap();
        let last = match last {
            Some(last) => Some(last),
            // After a restart: when was the last one written?
            None => sqlx::query_as::<_, (Option<OffsetDateTime>,)>("SELECT max(sampled_at) FROM metric_samples")
                .fetch_one(&self.db)
                .await
                .ok()
                .and_then(|(at,)| at),
        };
        if !sample_due(last, now) {
            *self.last_sample.lock().unwrap() = last;
            return;
        }
        let host = snapshot.host.as_ref();
        let memory = host.and_then(|h| h.memory);
        let fullest = host.and_then(|h| h.disks.iter().map(|d| d.used_percent).reduce(f32::max));
        let written = sqlx::query(
            "INSERT INTO metric_samples (sampled_at, cpu_percent, load1, memory_percent, swap_percent, disk_percent, devices)
             VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (sampled_at) DO NOTHING",
        )
        .bind(now)
        .bind(host.and_then(|h| h.cpu.percent))
        .bind(host.and_then(|h| h.cpu.load1))
        .bind(memory.map(|m| m.used_percent()))
        .bind(memory.and_then(|m| m.swap_used_percent()))
        .bind(fullest)
        // Sessions TAK Server lists with their account; the leftovers it lists without are not devices.
        .bind(snapshot.tak.api_ok.then_some(snapshot.devices.identified as i32))
        .execute(&self.db)
        .await;
        if let Err(e) = written {
            tracing::warn!(error = %e, "history sample not written");
            return;
        }
        *self.last_sample.lock().unwrap() = Some(now);

        let measured: Vec<&ServiceView> =
            snapshot.services.services.iter().filter(|s| s.cpu_percent.is_some() || s.memory_bytes.is_some() || s.disk_bytes.is_some()).collect();
        if measured.is_empty() {
            return;
        }
        let bytes = |b: Option<u64>| b.and_then(|b| i64::try_from(b).ok());
        let written = sqlx::query(
            "INSERT INTO service_samples (sampled_at, service, cpu_percent, memory_bytes, disk_bytes)
             SELECT $1, * FROM UNNEST($2::text[], $3::real[], $4::bigint[], $5::bigint[])
             ON CONFLICT (sampled_at, service) DO NOTHING",
        )
        .bind(now)
        .bind(measured.iter().map(|s| s.service.clone()).collect::<Vec<String>>())
        .bind(measured.iter().map(|s| s.cpu_percent).collect::<Vec<Option<f32>>>())
        .bind(measured.iter().map(|s| bytes(s.memory_bytes)).collect::<Vec<Option<i64>>>())
        .bind(measured.iter().map(|s| bytes(s.disk_bytes)).collect::<Vec<Option<i64>>>())
        .execute(&self.db)
        .await;
        if let Err(e) = written {
            tracing::warn!(error = %e, "history sample of the services not written");
        }
    }
}

/// Whether a look at `now` is kept as a history sample, the last one being at `last`.
fn sample_due(last: Option<OffsetDateTime>, now: OffsetDateTime) -> bool {
    last.is_none_or(|last| now - last >= SAMPLE_EVERY - SAMPLE_SLACK)
}

/// Looks at everything every `interval`, for ever. Started once, with the server.
pub async fn run_periodically(monitor: Arc<Monitor>) {
    // Let the reconciliation make its first pass before judging it.
    tokio::time::sleep(Duration::from_secs(3)).await;
    loop {
        if let Err(e) = monitor.run_cycle().await {
            tracing::error!(error = %format!("{e:#}"), "monitoring look failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(monitor.targets.interval) => {}
            _ = monitor.kick.notified() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn restarts_are_counted_over_a_quarter_of_an_hour() {
        let start = datetime!(2026-11-19 12:00 UTC);
        let at = |minutes: i64| start + time::Duration::minutes(minutes);
        let mut history = RestartHistory::default();
        assert_eq!(history.record("takserver", 4, at(0)), 0, "old restarts are not news");
        assert_eq!(history.record("takserver", 4, at(1)), 0);
        assert_eq!(history.record("takserver", 5, at(2)), 1);
        assert_eq!(history.record("takserver", 7, at(3)), 3, "three within the window: a loop");
        // It then stays up: the restarts leave the window.
        assert_eq!(history.record("takserver", 7, at(17)), 2, "the first one is now more than 15 minutes old");
        assert_eq!(history.record("takserver", 7, at(19)), 0);
        // Another service has its own count.
        assert_eq!(history.record("caddy", 0, at(19)), 0);
    }

    #[test]
    fn one_sample_a_minute_even_when_looks_drift() {
        let start = datetime!(2026-11-20 12:00 UTC);
        let at = |seconds: i64| start + time::Duration::seconds(seconds);
        assert!(sample_due(None, at(0)), "the first look after a start");
        assert!(sample_due(Some(at(0)), at(60)));
        assert!(sample_due(Some(at(0)), at(58)), "a look that comes a little early is still the next minute's");
        assert!(!sample_due(Some(at(0)), at(20)), "an administrator's refresh in between");
        assert!(sample_due(Some(at(0)), at(300)), "after a pause");
    }

    #[test]
    fn every_client_port_is_watched() {
        let views = |input_open, published: Option<&[u16]>| -> Vec<(u16, bool, Option<bool>)> {
            client_port_views(&[8089, 4443, 8090], input_open, published).iter().map(|p| (p.port, p.open, p.published)).collect()
        };
        assert_eq!(views(true, Some(&[4443, 8089, 8090])), vec![(8089, true, Some(true)), (4443, true, Some(true)), (8090, true, Some(true))]);
        assert_eq!(
            views(true, Some(&[8089])),
            vec![(8089, true, Some(true)), (4443, false, Some(false)), (8090, false, Some(false))],
            "TAK_CLIENT_PORTS changed without ./install.sh: Docker still publishes the old list"
        );
        assert_eq!(views(false, Some(&[8089, 4443, 8090])).iter().filter(|p| p.1).count(), 0, "the input is down: none answers");
        assert_eq!(views(true, None), vec![(8089, true, None), (4443, true, None), (8090, true, None)], "without the agent, the input alone decides");
    }

    #[test]
    fn the_ports_published_to_tak_servers_input_are_read_from_the_agents_report() {
        use teamtak_agent::report::{ContainerReport, ContainersReport, PublishedPort};
        let port = |container_port, protocol: &str, host_port| PublishedPort { container_port, protocol: protocol.into(), host_port };
        let takserver = ContainerReport {
            service: "takserver".into(),
            published_ports: Some(vec![port(8089, "tcp", 4443), port(8089, "tcp", 8089), port(8089, "udp", 8089), port(8446, "tcp", 8446)]),
            ..Default::default()
        };
        let report = |containers: Vec<ContainerReport>, error: Option<&str>| Report {
            agent_version: "1.0.4".into(),
            generated_at: datetime!(2026-11-20 12:00 UTC),
            mode: "production".into(),
            host: Default::default(),
            containers: ContainersReport { expected: vec![], containers, error: error.map(str::to_string) },
            tak: Default::default(),
            backups: Default::default(),
            letsencrypt: Default::default(),
        };
        assert_eq!(published_to_input(&report(vec![takserver.clone()], None)), Some(vec![4443, 8089]));
        assert_eq!(published_to_input(&report(vec![takserver.clone()], Some("docker: unavailable"))), None, "Docker did not answer");
        assert_eq!(published_to_input(&report(vec![], None)), None, "no TAK Server container");
        let older_agent = ContainerReport { published_ports: None, ..takserver };
        assert_eq!(published_to_input(&report(vec![older_agent], None)), None, "an agent that does not report them");
    }

    #[test]
    fn a_container_created_again_starts_from_zero() {
        let start = datetime!(2026-11-19 12:00 UTC);
        let mut history = RestartHistory::default();
        history.record("app", 9, start);
        assert_eq!(history.record("app", 0, start + time::Duration::minutes(1)), 0, "docker compose up -d recreated it");
        assert_eq!(history.record("app", 1, start + time::Duration::minutes(2)), 1);
        history.keep_only(&["caddy"]);
        assert_eq!(history.record("app", 5, start + time::Duration::minutes(3)), 0, "forgotten with the service");
    }
}
