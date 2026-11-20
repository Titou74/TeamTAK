//! Alerts: what is wrong, how bad, and when to say it.
//!
//! Two pure steps, so the rules can be tested without a machine to break:
//!
//! 1. [`evaluate`] reads the latest [`Observations`] and lists the [`Finding`]s: one per
//!    thing that is wrong now. It also says which kinds of alert it was able to judge: a
//!    kind whose source could not be read (the agent is down, the distribution is unknown)
//!    is neither raised nor resolved.
//! 2. [`plan`] compares the findings with the alerts already open and decides what to do:
//!    raise, escalate, remind, resolve. A problem must be seen on consecutive evaluations
//!    before it is raised, and be gone on consecutive evaluations before it is resolved, so
//!    a service that restarts does not send two e-mails.
//!
//! Notifications follow from the transitions: one when an alert is raised (and one more if
//! it becomes more serious), one when it is resolved, a reminder at most once per
//! [`Policy::reminder`] while it lasts. The caller sends all the transitions of one
//! evaluation in a single message, so everything failing at once is one e-mail, not twenty.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};

use crate::versions::{parse_release_version, release_is_newer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing, nothing is broken: updates to apply, a reboot to plan.
    Low,
    /// Something needs attention before it breaks.
    Warning,
    /// Users are affected, or will be very soon.
    Critical,
}

impl Severity {
    pub const ALL: &'static [Severity] = &[Severity::Low, Severity::Warning, Severity::Critical];

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Warning => "warning",
            Severity::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

macro_rules! alert_kinds {
    ($( $(#[$doc:meta])* $variant:ident => $code:literal ),+ $(,)?) => {
        /// Every kind of alert. The code is stable: it is stored, sent to the webhook, and
        /// translated by the UI (`alerts.kinds.<code>`) and by the e-mails.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum AlertKind {
            $( $(#[$doc])* $variant ),+
        }

        impl AlertKind {
            pub const ALL: &'static [AlertKind] = &[ $( AlertKind::$variant ),+ ];

            pub fn code(self) -> &'static str {
                match self { $( AlertKind::$variant => $code ),+ }
            }

            pub fn parse(code: &str) -> Option<Self> {
                match code { $( $code => Some(AlertKind::$variant), )+ _ => None }
            }
        }
    };
}

alert_kinds! {
    /// The platform cannot reach its host agent: the machine and the services are not watched.
    AgentUnreachable => "agent_unreachable",
    /// A filesystem is nearly full. Subject: the filesystem.
    DiskFull => "disk_full",
    MemoryPressure => "memory_pressure",
    /// A service of the stack is stopped or missing. Subject: the service.
    ServiceDown => "service_down",
    /// A service runs but its healthcheck fails. Subject: the service.
    ServiceUnhealthy => "service_unhealthy",
    /// A service keeps restarting. Subject: the service.
    ServiceRestarting => "service_restarting",
    /// One of TAK Server's processes is gone. Subject: the process.
    TakProcessDown => "tak_process_down",
    /// A port of TAK Server does not answer, or its API refuses the platform.
    TakUnreachable => "tak_unreachable",
    DirectoryUnreachable => "directory_unreachable",
    /// No reconciliation pass has ended without error for a while.
    ReconciliationFailing => "reconciliation_failing",
    /// An event's start was not applied. Subject: the event's group.
    EventNotStarted => "event_not_started",
    /// An event's end was not applied: its teams still see each other. Subject: the group.
    EventNotEnded => "event_not_ended",
    /// A certificate of the platform itself nears its end. Subject: which one.
    CertificateExpiring => "certificate_expiring",
    /// A certificate of the platform itself is past its end. Subject: which one.
    CertificateExpired => "certificate_expired",
    /// Client certificates near their end (their team leaders are told too).
    ClientCertificatesExpiring => "client_certificates_expiring",
    BackupFailed => "backup_failed",
    /// Backups are on and the newest archive is too old.
    BackupStale => "backup_stale",
    SecurityUpdates => "security_updates",
    RebootRequired => "reboot_required",
    /// A newer release of TeamTAK is published on GitHub.
    TeamtakUpdate => "teamtak_update",
    /// E-mails are not going out.
    MailFailing => "mail_failing",
    ClockUnsynchronized => "clock_unsynchronized",
}

/// Limits an administrator can change. The defaults suit a small server.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// A filesystem is "nearly full" from this share of use, in percent.
    pub disk_percent: f32,
    /// Memory in use (everything the kernel cannot reclaim), in percent.
    pub memory_percent: f32,
    /// A certificate is announced this many days before it expires…
    pub certificate_warning_days: i64,
    /// …and becomes critical this many days before.
    pub certificate_critical_days: i64,
    /// With daily backups, the newest archive may not be older than this.
    pub backup_max_age_hours: i64,
    /// E-mails that failed within an hour before it is an alert.
    pub mail_failures_per_hour: u32,
    /// Restarts of one service within the window that make a loop.
    pub restart_loop_count: u32,
    /// Minutes without a clean reconciliation pass.
    pub reconcile_stale_minutes: i64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            disk_percent: 90.0,
            memory_percent: 92.0,
            certificate_warning_days: 30,
            certificate_critical_days: 7,
            backup_max_age_hours: 36,
            mail_failures_per_hour: 3,
            restart_loop_count: 3,
            reconcile_stale_minutes: 10,
        }
    }
}

impl Thresholds {
    /// Brings every value into a range that still makes sense, so a typing mistake cannot
    /// silence an alert for good or make it permanent.
    pub fn sanitized(mut self) -> Self {
        let finite = |v: f32, default: f32| if v.is_finite() { v } else { default };
        let d = Self::default();
        self.disk_percent = finite(self.disk_percent, d.disk_percent).clamp(50.0, 99.0);
        self.memory_percent = finite(self.memory_percent, d.memory_percent).clamp(50.0, 99.0);
        self.certificate_critical_days = self.certificate_critical_days.clamp(1, 90);
        self.certificate_warning_days = self.certificate_warning_days.clamp(self.certificate_critical_days, 365);
        self.backup_max_age_hours = self.backup_max_age_hours.clamp(25, 24 * 30);
        self.mail_failures_per_hour = self.mail_failures_per_hour.clamp(1, 1000);
        self.restart_loop_count = self.restart_loop_count.clamp(2, 100);
        self.reconcile_stale_minutes = self.reconcile_stale_minutes.clamp(3, 24 * 60);
        self
    }
}

/// How long after its scheduled time a start or an end counts as not applied.
pub const EVENT_GRACE: Duration = Duration::minutes(2);

// ------------------------------------------------------------------------------- observations

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// No agent is configured: nothing about the machine is known, and that is not an alert.
    Disabled,
    Unreachable,
    Ok,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceObservation {
    pub name: String,
    /// Docker's state; `None` when the stack has no container for the service.
    pub state: Option<String>,
    pub health: Option<String>,
    /// Restarts counted within the recent window.
    pub recent_restarts: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CertificateObservation {
    /// `root_ca`, `intermediate_ca`, `tak_server`, `platform_api`, `tak_admin_ui`,
    /// `enrollment`, `web`.
    pub name: String,
    pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OverdueEvent {
    pub group: String,
    pub name: String,
    pub boundary: Boundary,
    pub due_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BackupObservation {
    pub enabled: bool,
    /// When the newest archive was written.
    pub newest_at: Option<OffsetDateTime>,
    /// Since when backups could have been made (the platform's installation).
    pub since: Option<OffsetDateTime>,
    /// The latest run ended in failure.
    pub last_run_failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MailObservation {
    pub failures_last_hour: u32,
    pub last_failure_at: Option<OffsetDateTime>,
    pub last_success_at: Option<OffsetDateTime>,
}

/// TeamTAK's own version and the latest one published.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReleaseObservation {
    /// The version that runs.
    pub installed: String,
    /// The latest release published; `None` when none is, or the check is turned off.
    pub latest: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ReconcileObservation {
    pub last_ok_at: Option<OffsetDateTime>,
    /// When the platform (this process) started: a pass cannot be older.
    pub running_since: Option<OffsetDateTime>,
}

/// Everything the rules look at. `None` means "could not be read", which is different from
/// "fine".
#[derive(Debug, Clone, PartialEq)]
pub struct Observations {
    pub now: OffsetDateTime,
    pub agent: AgentState,
    /// `(name, used percent)` of each filesystem.
    pub disks: Option<Vec<(String, f32)>>,
    pub memory_used_percent: Option<f32>,
    pub services: Option<Vec<ServiceObservation>>,
    /// `(name, running)` of TAK Server's processes.
    pub tak_processes: Option<Vec<(String, bool)>>,
    pub security_updates: Option<u32>,
    pub reboot_required: Option<bool>,
    /// `None` until the releases could be asked once.
    pub release: Option<ReleaseObservation>,
    pub clock_synchronized: Option<bool>,
    /// What does not answer on TAK Server, by name (`8089`, `8443`, `8446`, `api`).
    pub tak_failures: Option<Vec<String>>,
    pub directory_reachable: Option<bool>,
    pub reconcile: Option<ReconcileObservation>,
    pub overdue_events: Option<Vec<OverdueEvent>>,
    pub certificates: Option<Vec<CertificateObservation>>,
    /// `(expiring within the warning period, within the critical one)`.
    pub client_certificates: Option<(u32, u32)>,
    pub backups: Option<BackupObservation>,
    pub mail: Option<MailObservation>,
}

impl Observations {
    /// Nothing known yet.
    pub fn empty(now: OffsetDateTime) -> Self {
        Self {
            now,
            agent: AgentState::Disabled,
            disks: None,
            memory_used_percent: None,
            services: None,
            tak_processes: None,
            security_updates: None,
            reboot_required: None,
            release: None,
            clock_synchronized: None,
            tak_failures: None,
            directory_reachable: None,
            reconcile: None,
            overdue_events: None,
            certificates: None,
            client_certificates: None,
            backups: None,
            mail: None,
        }
    }
}

// ------------------------------------------------------------------------------- findings

/// One thing that is wrong now.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub kind: AlertKind,
    /// What it is about when a kind can have several at once (a service, a filesystem);
    /// empty otherwise.
    pub subject: String,
    pub severity: Severity,
    /// Numbers and names the texts are filled with. Never free text, never a secret.
    pub details: BTreeMap<String, Value>,
}

impl Finding {
    fn new(kind: AlertKind, subject: impl Into<String>, severity: Severity) -> Self {
        Self { kind, subject: subject.into(), severity, details: BTreeMap::new() }
    }

    fn with(mut self, key: &str, value: Value) -> Self {
        self.details.insert(key.to_string(), value);
        self
    }

    pub fn key(&self) -> AlertKey {
        (self.kind, self.subject.clone())
    }
}

pub type AlertKey = (AlertKind, String);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Evaluation {
    pub findings: Vec<Finding>,
    /// Kinds the observations allowed to judge. An open alert of another kind is left as it
    /// is: not seeing a problem is not the same as seeing it gone.
    pub judged: BTreeSet<AlertKind>,
}

fn whole_days_left(expires_at: OffsetDateTime, now: OffsetDateTime) -> i64 {
    (expires_at - now).whole_seconds().div_euclid(86_400)
}

/// Lists what is wrong in `obs`.
pub fn evaluate(obs: &Observations, thresholds: &Thresholds) -> Evaluation {
    use AlertKind::*;
    let t = thresholds.sanitized();
    let mut out = Evaluation::default();
    // One decimal, as a 64-bit number: a 32-bit 99.2 would be written 99.19999694824219.
    let round = |v: f32| json!((f64::from(v) * 10.0).round() / 10.0);

    // The agent itself. Without it nothing below that comes from the machine is judged.
    out.judged.insert(AgentUnreachable);
    if obs.agent == AgentState::Unreachable {
        out.findings.push(Finding::new(AgentUnreachable, "", Severity::Warning));
    }

    if let Some(disks) = &obs.disks {
        out.judged.insert(DiskFull);
        for (name, used) in disks {
            if *used >= t.disk_percent {
                // Five more points, or 97 % whatever the threshold: little room is left.
                let critical = *used >= (t.disk_percent + 5.0).min(97.0).max(t.disk_percent);
                let severity = if critical { Severity::Critical } else { Severity::Warning };
                out.findings.push(Finding::new(DiskFull, name.clone(), severity).with("percent", round(*used)));
            }
        }
    }

    if let Some(used) = obs.memory_used_percent {
        out.judged.insert(MemoryPressure);
        if used >= t.memory_percent {
            out.findings.push(Finding::new(MemoryPressure, "", Severity::Warning).with("percent", round(used)));
        }
    }

    if let Some(services) = &obs.services {
        out.judged.extend([ServiceDown, ServiceUnhealthy, ServiceRestarting]);
        for service in services {
            let looping = service.recent_restarts >= t.restart_loop_count;
            match service.state.as_deref() {
                Some("restarting") => out.findings.push(
                    Finding::new(ServiceRestarting, service.name.clone(), Severity::Critical).with("restarts", json!(service.recent_restarts)),
                ),
                Some("running") if looping => out.findings.push(
                    Finding::new(ServiceRestarting, service.name.clone(), Severity::Critical).with("restarts", json!(service.recent_restarts)),
                ),
                Some("running") => {
                    if service.health.as_deref() == Some("unhealthy") {
                        out.findings.push(Finding::new(ServiceUnhealthy, service.name.clone(), Severity::Warning));
                    }
                }
                Some(state) => out
                    .findings
                    .push(Finding::new(ServiceDown, service.name.clone(), Severity::Critical).with("state", json!(state))),
                None => out
                    .findings
                    .push(Finding::new(ServiceDown, service.name.clone(), Severity::Critical).with("state", json!("missing"))),
            }
        }
    }

    if let Some(processes) = &obs.tak_processes {
        out.judged.insert(TakProcessDown);
        for (name, running) in processes {
            if !running {
                out.findings.push(Finding::new(TakProcessDown, name.clone(), Severity::Critical));
            }
        }
    }

    if let Some(count) = obs.security_updates {
        out.judged.insert(SecurityUpdates);
        if count > 0 {
            out.findings.push(Finding::new(SecurityUpdates, "", Severity::Low).with("count", json!(count)));
        }
    }
    if let Some(required) = obs.reboot_required {
        out.judged.insert(RebootRequired);
        if required {
            out.findings.push(Finding::new(RebootRequired, "", Severity::Low));
        }
    }
    if let Some(release) = &obs.release {
        out.judged.insert(TeamtakUpdate);
        if let Some(latest) = release.latest.as_deref().filter(|latest| release_is_newer(latest, &release.installed)) {
            let latest = parse_release_version(latest).map(|v| v.to_string()).unwrap_or_default();
            out.findings.push(Finding::new(TeamtakUpdate, "", Severity::Low).with("version", json!(latest)).with("installed", json!(release.installed)));
        }
    }
    if let Some(synchronized) = obs.clock_synchronized {
        out.judged.insert(ClockUnsynchronized);
        if !synchronized {
            out.findings.push(Finding::new(ClockUnsynchronized, "", Severity::Warning));
        }
    }

    if let Some(failures) = &obs.tak_failures {
        out.judged.insert(TakUnreachable);
        if !failures.is_empty() {
            out.findings.push(Finding::new(TakUnreachable, "", Severity::Critical).with("what", json!(failures.join(", "))));
        }
    }
    if let Some(reachable) = obs.directory_reachable {
        out.judged.insert(DirectoryUnreachable);
        if !reachable {
            out.findings.push(Finding::new(DirectoryUnreachable, "", Severity::Critical));
        }
    }

    if let Some(reconcile) = &obs.reconcile {
        out.judged.insert(ReconciliationFailing);
        // Since the last clean pass, or since the platform started when there never was one.
        let reference = reconcile.last_ok_at.or(reconcile.running_since);
        if let Some(reference) = reference {
            let minutes = (obs.now - reference).whole_minutes();
            if minutes >= t.reconcile_stale_minutes {
                let severity = if minutes >= t.reconcile_stale_minutes * 6 { Severity::Critical } else { Severity::Warning };
                out.findings.push(Finding::new(ReconciliationFailing, "", severity).with("minutes", json!(minutes)));
            }
        }
    }

    if let Some(events) = &obs.overdue_events {
        out.judged.extend([EventNotStarted, EventNotEnded]);
        for event in events {
            if obs.now - event.due_at < EVENT_GRACE {
                continue;
            }
            // An event that does not end keeps teams visible to each other: the worse of the two.
            let (kind, severity) = match event.boundary {
                Boundary::Start => (EventNotStarted, Severity::Warning),
                Boundary::End => (EventNotEnded, Severity::Critical),
            };
            out.findings.push(
                Finding::new(kind, event.group.clone(), severity)
                    .with("event", json!(event.name))
                    .with("minutes", json!((obs.now - event.due_at).whole_minutes())),
            );
        }
    }

    if let Some(certificates) = &obs.certificates {
        out.judged.extend([CertificateExpiring, CertificateExpired]);
        for certificate in certificates {
            let days = whole_days_left(certificate.expires_at, obs.now);
            if certificate.expires_at <= obs.now {
                // "days" is then how many whole days ago.
                let finding = Finding::new(CertificateExpired, certificate.name.clone(), Severity::Critical);
                out.findings.push(finding.with("days", json!((obs.now - certificate.expires_at).whole_days())));
            } else if days <= t.certificate_warning_days {
                let severity = if days <= t.certificate_critical_days { Severity::Critical } else { Severity::Warning };
                out.findings.push(Finding::new(CertificateExpiring, certificate.name.clone(), severity).with("days", json!(days)));
            }
        }
    }
    if let Some((soon, very_soon)) = obs.client_certificates {
        out.judged.insert(ClientCertificatesExpiring);
        if soon > 0 || very_soon > 0 {
            let severity = if very_soon > 0 { Severity::Warning } else { Severity::Low };
            out.findings.push(
                Finding::new(ClientCertificatesExpiring, "", severity)
                    .with("count", json!(soon.max(very_soon)))
                    .with("urgent", json!(very_soon))
                    .with("days", json!(t.certificate_warning_days))
                    .with("urgent_days", json!(t.certificate_critical_days)),
            );
        }
    }

    if let Some(backups) = &obs.backups {
        out.judged.extend([BackupFailed, BackupStale]);
        if backups.enabled {
            if backups.last_run_failed {
                out.findings.push(Finding::new(BackupFailed, "", Severity::Warning));
            }
            if let Some(reference) = backups.newest_at.or(backups.since) {
                let hours = (obs.now - reference).whole_hours();
                if hours >= t.backup_max_age_hours {
                    let finding = Finding::new(BackupStale, "", Severity::Warning).with("hours", json!(hours));
                    out.findings.push(finding.with("never", json!(backups.newest_at.is_none())));
                }
            }
        }
    }

    if let Some(mail) = &obs.mail {
        out.judged.insert(MailFailing);
        // Failing now: enough failures lately, and none of the later attempts went through.
        let still_failing = match (mail.last_failure_at, mail.last_success_at) {
            (Some(failure), Some(success)) => failure > success,
            (Some(_), None) => true,
            _ => false,
        };
        if still_failing && mail.failures_last_hour >= t.mail_failures_per_hour {
            out.findings.push(Finding::new(MailFailing, "", Severity::Warning).with("count", json!(mail.failures_last_hour)));
        }
    }

    out
}

// ------------------------------------------------------------------------------- transitions

/// An alert that is open: raised and not resolved.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenAlert {
    pub id: i64,
    pub kind: AlertKind,
    pub subject: String,
    pub severity: Severity,
    pub last_notified_at: Option<OffsetDateTime>,
}

impl OpenAlert {
    fn key(&self) -> AlertKey {
        (self.kind, self.subject.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Policy {
    /// Consecutive evaluations a problem must be seen on before it is raised.
    pub raise_after: u32,
    /// Consecutive evaluations it must be gone on before the alert is resolved.
    pub resolve_after: u32,
    /// Shortest time between two reminders of the same alert.
    pub reminder: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self { raise_after: 2, resolve_after: 2, reminder: Duration::hours(24) }
    }
}

/// How many evaluations in a row each problem was seen, or each open alert was seen gone.
/// Kept in memory between evaluations: losing it at a restart only delays a transition.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Streaks {
    seen: BTreeMap<AlertKey, u32>,
    gone: BTreeMap<AlertKey, u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    /// A new alert. Notified.
    Raise(Finding),
    /// An open alert became more serious. Notified.
    Escalate { id: i64, finding: Finding },
    /// An open alert is still there; its figures are refreshed. Not notified.
    Refresh { id: i64, finding: Finding },
    /// An open alert is still there and was last mentioned long enough ago. Notified.
    Remind { id: i64, finding: Finding },
    /// An open alert is over. Notified.
    Resolve { id: i64 },
}

impl Transition {
    /// True when the administrators are told about it.
    pub fn notifies(&self) -> bool {
        !matches!(self, Transition::Refresh { .. })
    }
}

/// Decides what to do with the open alerts given what is wrong now.
pub fn plan(open: &[OpenAlert], evaluation: &Evaluation, streaks: &mut Streaks, policy: &Policy, now: OffsetDateTime) -> Vec<Transition> {
    let mut transitions = Vec::new();
    let found: BTreeMap<AlertKey, &Finding> = evaluation.findings.iter().map(|f| (f.key(), f)).collect();
    let open_keys: BTreeSet<AlertKey> = open.iter().map(OpenAlert::key).collect();

    // Problems that are not alerts yet.
    for (key, finding) in &found {
        if open_keys.contains(key) {
            continue;
        }
        let seen = streaks.seen.entry(key.clone()).or_insert(0);
        *seen += 1;
        if *seen >= policy.raise_after.max(1) {
            transitions.push(Transition::Raise((*finding).clone()));
        }
    }
    // A problem that went away before being raised starts again from nothing; one that was
    // just raised no longer needs counting.
    let raised: BTreeSet<AlertKey> = transitions
        .iter()
        .filter_map(|t| match t {
            Transition::Raise(f) => Some(f.key()),
            _ => None,
        })
        .collect();
    streaks.seen.retain(|key, _| found.contains_key(key) && !open_keys.contains(key) && !raised.contains(key));

    for alert in open {
        let key = alert.key();
        match found.get(&key) {
            Some(finding) => {
                streaks.gone.remove(&key);
                let finding = (*finding).clone();
                let due = alert.last_notified_at.is_none_or(|at| now - at >= policy.reminder);
                if finding.severity > alert.severity {
                    transitions.push(Transition::Escalate { id: alert.id, finding });
                } else if due {
                    transitions.push(Transition::Remind { id: alert.id, finding });
                } else {
                    transitions.push(Transition::Refresh { id: alert.id, finding });
                }
            }
            // Its source could not be read this time: nothing is known, nothing changes.
            None if !evaluation.judged.contains(&alert.kind) => {}
            None => {
                let gone = streaks.gone.entry(key.clone()).or_insert(0);
                *gone += 1;
                if *gone >= policy.resolve_after.max(1) {
                    streaks.gone.remove(&key);
                    transitions.push(Transition::Resolve { id: alert.id });
                }
            }
        }
    }
    streaks.gone.retain(|key, _| open_keys.contains(key));
    transitions
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const NOW: OffsetDateTime = datetime!(2026-11-19 12:00 UTC);

    fn healthy() -> Observations {
        Observations {
            agent: AgentState::Ok,
            disks: Some(vec![("root".into(), 41.0), ("docker".into(), 63.5)]),
            memory_used_percent: Some(55.0),
            services: Some(
                ["caddy", "app", "takserver"]
                    .iter()
                    .map(|name| ServiceObservation { name: name.to_string(), state: Some("running".into()), health: Some("healthy".into()), recent_restarts: 0 })
                    .collect(),
            ),
            tak_processes: Some(vec![("api".into(), true), ("messaging".into(), true)]),
            security_updates: Some(0),
            reboot_required: Some(false),
            release: Some(ReleaseObservation { installed: "1.0.1".into(), latest: Some("v1.0.1".into()) }),
            clock_synchronized: Some(true),
            tak_failures: Some(vec![]),
            directory_reachable: Some(true),
            reconcile: Some(ReconcileObservation { last_ok_at: Some(NOW - Duration::seconds(40)), running_since: Some(NOW - Duration::days(3)) }),
            overdue_events: Some(vec![]),
            certificates: Some(vec![CertificateObservation { name: "tak_server".into(), expires_at: NOW + Duration::days(400) }]),
            client_certificates: Some((0, 0)),
            backups: Some(BackupObservation { enabled: true, newest_at: Some(NOW - Duration::hours(9)), since: Some(NOW - Duration::days(90)), last_run_failed: false }),
            mail: Some(MailObservation::default()),
            ..Observations::empty(NOW)
        }
    }

    fn kinds(e: &Evaluation) -> Vec<(&'static str, String, Severity)> {
        e.findings.iter().map(|f| (f.kind.code(), f.subject.clone(), f.severity)).collect()
    }

    fn found(obs: &Observations) -> Vec<(&'static str, String, Severity)> {
        kinds(&evaluate(obs, &Thresholds::default()))
    }

    #[test]
    fn a_healthy_platform_has_no_finding_and_everything_is_judged() {
        let e = evaluate(&healthy(), &Thresholds::default());
        assert_eq!(e.findings, vec![]);
        for kind in AlertKind::ALL {
            assert!(e.judged.contains(kind), "{} must be judged when everything could be read", kind.code());
        }
    }

    #[test]
    fn codes_are_unique_and_round_trip() {
        let codes: BTreeSet<&str> = AlertKind::ALL.iter().map(|k| k.code()).collect();
        assert_eq!(codes.len(), AlertKind::ALL.len());
        for kind in AlertKind::ALL {
            assert_eq!(AlertKind::parse(kind.code()), Some(*kind));
            assert!(kind.code().bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
        assert_eq!(AlertKind::parse("nope"), None);
        assert!(Severity::Low < Severity::Warning && Severity::Warning < Severity::Critical);
        assert_eq!(Severity::parse("critical"), Some(Severity::Critical));
    }

    #[test]
    fn disk_thresholds() {
        let mut obs = healthy();
        obs.disks = Some(vec![("root".into(), 89.9), ("docker".into(), 90.0), ("data".into(), 95.0), ("backups".into(), 99.2)]);
        assert_eq!(
            found(&obs),
            vec![
                ("disk_full", "docker".to_string(), Severity::Warning),
                ("disk_full", "data".to_string(), Severity::Critical),
                ("disk_full", "backups".to_string(), Severity::Critical),
            ]
        );
        // A threshold of 98 %: critical cannot be asked more than 97 %, so it is critical at once.
        let strict = Thresholds { disk_percent: 98.0, ..Default::default() };
        let e = evaluate(&obs, &strict);
        assert_eq!(kinds(&e), vec![("disk_full", "backups".to_string(), Severity::Critical)]);
        assert_eq!(e.findings[0].details["percent"], json!(99.2));
    }

    #[test]
    fn memory_threshold_is_configurable() {
        let mut obs = healthy();
        obs.memory_used_percent = Some(91.0);
        assert_eq!(found(&obs), vec![]);
        obs.memory_used_percent = Some(92.0);
        assert_eq!(found(&obs), vec![("memory_pressure", String::new(), Severity::Warning)]);
        let relaxed = Thresholds { memory_percent: 97.0, ..Default::default() };
        assert_eq!(evaluate(&obs, &relaxed).findings, vec![]);
    }

    #[test]
    fn services_down_unhealthy_and_looping() {
        let service = |name: &str, state: Option<&str>, health: Option<&str>, restarts: u32| ServiceObservation {
            name: name.into(),
            state: state.map(str::to_string),
            health: health.map(str::to_string),
            recent_restarts: restarts,
        };
        let mut obs = healthy();
        obs.services = Some(vec![
            service("caddy", Some("exited"), None, 0),
            service("postgres", None, None, 0),
            service("openldap", Some("running"), Some("unhealthy"), 0),
            service("takserver", Some("running"), Some("starting"), 3),
            service("app", Some("restarting"), None, 1),
            service("mailpit", Some("running"), Some("starting"), 2),
            service("agent", Some("running"), None, 0),
        ]);
        assert_eq!(
            found(&obs),
            vec![
                ("service_down", "caddy".to_string(), Severity::Critical),
                ("service_down", "postgres".to_string(), Severity::Critical),
                ("service_unhealthy", "openldap".to_string(), Severity::Warning),
                ("service_restarting", "takserver".to_string(), Severity::Critical),
                ("service_restarting", "app".to_string(), Severity::Critical),
            ],
            "a service that is starting, or restarted twice, is not an alert"
        );
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(e.findings[1].details["state"], json!("missing"));
    }

    #[test]
    fn tak_server_and_directory() {
        let mut obs = healthy();
        obs.tak_failures = Some(vec!["8089".into(), "api".into()]);
        obs.directory_reachable = Some(false);
        obs.tak_processes = Some(vec![("api".into(), true), ("messaging".into(), false)]);
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(
            kinds(&e),
            vec![
                ("tak_process_down", "messaging".to_string(), Severity::Critical),
                ("tak_unreachable", String::new(), Severity::Critical),
                ("directory_unreachable", String::new(), Severity::Critical),
            ]
        );
        assert_eq!(e.findings[1].details["what"], json!("8089, api"));
    }

    #[test]
    fn reconciliation_failing_after_the_configured_delay() {
        let mut obs = healthy();
        let ok = |minutes: i64| Some(ReconcileObservation { last_ok_at: Some(NOW - Duration::minutes(minutes)), running_since: Some(NOW - Duration::days(3)) });
        obs.reconcile = ok(9);
        assert_eq!(found(&obs), vec![]);
        obs.reconcile = ok(10);
        assert_eq!(found(&obs), vec![("reconciliation_failing", String::new(), Severity::Warning)]);
        obs.reconcile = ok(60);
        assert_eq!(found(&obs), vec![("reconciliation_failing", String::new(), Severity::Critical)]);
        // Never a clean pass: counted from the platform's start, so a fresh start is not an alert.
        obs.reconcile = Some(ReconcileObservation { last_ok_at: None, running_since: Some(NOW - Duration::minutes(2)) });
        assert_eq!(found(&obs), vec![]);
        obs.reconcile = Some(ReconcileObservation { last_ok_at: None, running_since: Some(NOW - Duration::minutes(15)) });
        assert_eq!(found(&obs).len(), 1);
    }

    #[test]
    fn an_event_not_applied_on_time() {
        let mut obs = healthy();
        let event = |boundary, seconds: i64| OverdueEvent { group: "evt-drill".into(), name: "Drill".into(), boundary, due_at: NOW - Duration::seconds(seconds) };
        // Within the grace period the scheduler is simply at work.
        obs.overdue_events = Some(vec![event(Boundary::Start, 30), event(Boundary::End, 119)]);
        assert_eq!(found(&obs), vec![]);
        obs.overdue_events = Some(vec![event(Boundary::Start, 300), event(Boundary::End, 600)]);
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(
            kinds(&e),
            vec![("event_not_started", "evt-drill".to_string(), Severity::Warning), ("event_not_ended", "evt-drill".to_string(), Severity::Critical)]
        );
        assert_eq!((e.findings[1].details["event"].clone(), e.findings[1].details["minutes"].clone()), (json!("Drill"), json!(10)));
    }

    #[test]
    fn certificate_expiry_at_thirty_and_seven_days() {
        let cert = |name: &str, days: i64, extra: Duration| CertificateObservation { name: name.into(), expires_at: NOW + Duration::days(days) + extra };
        let mut obs = healthy();
        obs.certificates = Some(vec![
            cert("root_ca", 3000, Duration::ZERO),
            cert("tak_server", 31, Duration::ZERO),
            cert("platform_api", 30, Duration::hours(5)),
            cert("web", 7, Duration::hours(23)),
            cert("enrollment", 6, Duration::hours(23)),
            cert("intermediate_ca", -2, Duration::ZERO),
        ]);
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(
            kinds(&e),
            vec![
                ("certificate_expiring", "platform_api".to_string(), Severity::Warning),
                ("certificate_expiring", "web".to_string(), Severity::Critical),
                ("certificate_expiring", "enrollment".to_string(), Severity::Critical),
                ("certificate_expired", "intermediate_ca".to_string(), Severity::Critical),
            ]
        );
        let days: Vec<i64> = e.findings.iter().map(|f| f.details["days"].as_i64().unwrap()).collect();
        assert_eq!(days, vec![30, 7, 6, 2], "whole days left; for an expired one, whole days since");
        // The day it expires it is still "expiring", in 0 days; an hour later it has expired.
        obs.certificates = Some(vec![cert("web", 0, Duration::hours(3)), cert("tak_server", 0, -Duration::hours(1))]);
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(
            kinds(&e),
            vec![("certificate_expiring", "web".to_string(), Severity::Critical), ("certificate_expired", "tak_server".to_string(), Severity::Critical)]
        );
        assert_eq!((e.findings[0].details["days"].clone(), e.findings[1].details["days"].clone()), (json!(0), json!(0)));
        obs.certificates = Some(vec![
            cert("root_ca", 3000, Duration::ZERO),
            cert("tak_server", 31, Duration::ZERO),
            cert("platform_api", 30, Duration::hours(5)),
            cert("web", 7, Duration::hours(23)),
            cert("enrollment", 6, Duration::hours(23)),
            cert("intermediate_ca", -2, Duration::ZERO),
        ]);
        // Other thresholds.
        let early = Thresholds { certificate_warning_days: 60, certificate_critical_days: 14, ..Default::default() };
        assert_eq!(evaluate(&obs, &early).findings.len(), 5);
    }

    #[test]
    fn client_certificates_are_one_alert_whatever_their_number() {
        let mut obs = healthy();
        obs.client_certificates = Some((12, 0));
        assert_eq!(found(&obs), vec![("client_certificates_expiring", String::new(), Severity::Low)]);
        obs.client_certificates = Some((12, 2));
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(kinds(&e), vec![("client_certificates_expiring", String::new(), Severity::Warning)]);
        assert_eq!((e.findings[0].details["count"].clone(), e.findings[0].details["urgent"].clone()), (json!(12), json!(2)));
    }

    #[test]
    fn backups_failed_or_too_old() {
        let mut obs = healthy();
        obs.backups = Some(BackupObservation { enabled: true, newest_at: Some(NOW - Duration::hours(35)), since: None, last_run_failed: false });
        assert_eq!(found(&obs), vec![]);
        obs.backups = Some(BackupObservation { enabled: true, newest_at: Some(NOW - Duration::hours(36)), since: None, last_run_failed: true });
        assert_eq!(
            found(&obs),
            vec![("backup_failed", String::new(), Severity::Warning), ("backup_stale", String::new(), Severity::Warning)]
        );
        // Never any archive: stale once the platform has been installed for longer than the limit.
        obs.backups = Some(BackupObservation { enabled: true, newest_at: None, since: Some(NOW - Duration::hours(2)), last_run_failed: false });
        assert_eq!(found(&obs), vec![]);
        obs.backups = Some(BackupObservation { enabled: true, newest_at: None, since: Some(NOW - Duration::days(4)), last_run_failed: false });
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(kinds(&e), vec![("backup_stale", String::new(), Severity::Warning)]);
        assert_eq!(e.findings[0].details["never"], json!(true));
        // Switched off on purpose: no alert, and open ones resolve.
        obs.backups = Some(BackupObservation { enabled: false, newest_at: None, since: Some(NOW - Duration::days(400)), last_run_failed: true });
        let e = evaluate(&obs, &Thresholds::default());
        assert!(e.findings.is_empty() && e.judged.contains(&AlertKind::BackupStale));
    }

    #[test]
    fn low_severity_for_updates_and_reboot() {
        let mut obs = healthy();
        obs.security_updates = Some(4);
        obs.reboot_required = Some(true);
        obs.clock_synchronized = Some(false);
        assert_eq!(
            found(&obs),
            vec![
                ("security_updates", String::new(), Severity::Low),
                ("reboot_required", String::new(), Severity::Low),
                ("clock_unsynchronized", String::new(), Severity::Warning),
            ]
        );
    }

    #[test]
    fn a_newer_release_of_teamtak_is_a_low_alert_until_it_is_installed() {
        let mut obs = healthy();
        let release = |installed: &str, latest: Option<&str>| Some(ReleaseObservation { installed: installed.into(), latest: latest.map(str::to_string) });
        obs.release = release("1.0.1", Some("v1.0.2"));
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(kinds(&e), vec![("teamtak_update", String::new(), Severity::Low)]);
        assert_eq!(e.findings[0].details["version"], json!("1.0.2"), "written without the tag's v");
        assert_eq!(e.findings[0].details["installed"], json!("1.0.1"));
        // Installed, or older than what runs: nothing, and an open alert resolves.
        for (installed, latest) in [("1.0.2", Some("v1.0.2")), ("1.1.0", Some("v1.0.2")), ("1.0.1", None), ("1.1.0-dev", Some("v1.0.2"))] {
            obs.release = release(installed, latest);
            let e = evaluate(&obs, &Thresholds::default());
            assert!(e.findings.is_empty() && e.judged.contains(&AlertKind::TeamtakUpdate), "{installed} {latest:?}");
        }
        // Never asked: not judged.
        obs.release = None;
        assert!(!evaluate(&obs, &Thresholds::default()).judged.contains(&AlertKind::TeamtakUpdate));
    }

    #[test]
    fn mail_failing_only_while_nothing_gets_through() {
        let mut obs = healthy();
        let at = |minutes: i64| Some(NOW - Duration::minutes(minutes));
        obs.mail = Some(MailObservation { failures_last_hour: 2, last_failure_at: at(1), last_success_at: at(30) });
        assert_eq!(found(&obs), vec![], "below the threshold");
        obs.mail = Some(MailObservation { failures_last_hour: 5, last_failure_at: at(1), last_success_at: at(30) });
        assert_eq!(found(&obs), vec![("mail_failing", String::new(), Severity::Warning)]);
        obs.mail = Some(MailObservation { failures_last_hour: 5, last_failure_at: at(10), last_success_at: at(2) });
        assert_eq!(found(&obs), vec![], "a later e-mail went out: it works again");
        obs.mail = Some(MailObservation { failures_last_hour: 5, last_failure_at: at(10), last_success_at: None });
        assert_eq!(found(&obs).len(), 1);
    }

    #[test]
    fn what_could_not_be_read_is_not_judged() {
        // The agent does not answer: only what the platform sees by itself is judged.
        let mut obs = healthy();
        obs.agent = AgentState::Unreachable;
        obs.disks = None;
        obs.memory_used_percent = None;
        obs.services = None;
        obs.tak_processes = None;
        obs.security_updates = None;
        obs.reboot_required = None;
        obs.clock_synchronized = None;
        obs.backups = None;
        let e = evaluate(&obs, &Thresholds::default());
        assert_eq!(kinds(&e), vec![("agent_unreachable", String::new(), Severity::Warning)]);
        for kind in [AlertKind::DiskFull, AlertKind::ServiceDown, AlertKind::SecurityUpdates, AlertKind::BackupStale, AlertKind::ClockUnsynchronized] {
            assert!(!e.judged.contains(&kind), "{}", kind.code());
        }
        for kind in [AlertKind::TakUnreachable, AlertKind::CertificateExpiring, AlertKind::ReconciliationFailing, AlertKind::MailFailing] {
            assert!(e.judged.contains(&kind), "{}", kind.code());
        }
        // No agent at all (not installed): not an alert.
        obs.agent = AgentState::Disabled;
        assert_eq!(evaluate(&obs, &Thresholds::default()).findings, vec![]);
    }

    #[test]
    fn thresholds_are_kept_within_reason() {
        let wild = Thresholds {
            disk_percent: 250.0,
            memory_percent: f32::NAN,
            certificate_warning_days: 2,
            certificate_critical_days: 400,
            backup_max_age_hours: 1,
            mail_failures_per_hour: 0,
            restart_loop_count: 0,
            reconcile_stale_minutes: 0,
        }
        .sanitized();
        assert_eq!((wild.disk_percent, wild.memory_percent), (99.0, 92.0));
        assert_eq!((wild.certificate_critical_days, wild.certificate_warning_days), (90, 90), "the warning never comes after the critical");
        assert_eq!((wild.backup_max_age_hours, wild.mail_failures_per_hour, wild.restart_loop_count, wild.reconcile_stale_minutes), (25, 1, 2, 3));
        assert_eq!(Thresholds::default().sanitized(), Thresholds::default());
        // Missing fields of stored settings take their default.
        let partial: Thresholds = serde_json::from_value(json!({ "disk_percent": 80.0 })).unwrap();
        assert_eq!(partial, Thresholds { disk_percent: 80.0, ..Default::default() });
    }

    // --------------------------------------------------------------------------- transitions

    fn finding(kind: AlertKind, subject: &str, severity: Severity) -> Finding {
        Finding::new(kind, subject, severity)
    }

    fn evaluation(findings: Vec<Finding>) -> Evaluation {
        Evaluation { findings, judged: AlertKind::ALL.iter().copied().collect() }
    }

    fn open(id: i64, kind: AlertKind, subject: &str, severity: Severity, notified_hours_ago: i64) -> OpenAlert {
        OpenAlert { id, kind, subject: subject.into(), severity, last_notified_at: Some(NOW - Duration::hours(notified_hours_ago)) }
    }

    #[test]
    fn a_problem_is_raised_once_it_is_seen_twice_in_a_row() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        let down = evaluation(vec![finding(AlertKind::ServiceDown, "caddy", Severity::Critical)]);
        assert_eq!(plan(&[], &down, &mut streaks, &policy, NOW), vec![], "seen once: it may be a restart");
        let second = plan(&[], &down, &mut streaks, &policy, NOW);
        assert_eq!(second, vec![Transition::Raise(finding(AlertKind::ServiceDown, "caddy", Severity::Critical))]);
        assert!(second[0].notifies());
        assert_eq!(streaks, Streaks::default(), "nothing left to count once raised");
    }

    #[test]
    fn a_blip_raises_nothing() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        let down = evaluation(vec![finding(AlertKind::ServiceDown, "caddy", Severity::Critical)]);
        let fine = evaluation(vec![]);
        for _ in 0..5 {
            assert_eq!(plan(&[], &down, &mut streaks, &policy, NOW), vec![]);
            assert_eq!(plan(&[], &fine, &mut streaks, &policy, NOW), vec![], "back to normal: the count starts again");
        }
    }

    #[test]
    fn an_open_alert_is_not_notified_again_until_the_reminder_is_due() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        let still = evaluation(vec![finding(AlertKind::DiskFull, "root", Severity::Warning)]);
        let recent = [open(7, AlertKind::DiskFull, "root", Severity::Warning, 23)];
        let t = plan(&recent, &still, &mut streaks, &policy, NOW);
        assert!(matches!(&t[..], [Transition::Refresh { id: 7, .. }]) && !t[0].notifies());
        let day_old = [open(7, AlertKind::DiskFull, "root", Severity::Warning, 24)];
        let t = plan(&day_old, &still, &mut streaks, &policy, NOW);
        assert!(matches!(&t[..], [Transition::Remind { id: 7, .. }]) && t[0].notifies());
        // A week of the same problem, looked at every minute: seven reminders, not ten thousand.
        let mut last_notified = NOW;
        let mut reminders = 0;
        for minute in 1..=(7 * 24 * 60) {
            let at = NOW + Duration::minutes(minute);
            let alert = [OpenAlert { id: 7, kind: AlertKind::DiskFull, subject: "root".into(), severity: Severity::Warning, last_notified_at: Some(last_notified) }];
            for t in plan(&alert, &still, &mut streaks, &policy, at) {
                if t.notifies() {
                    reminders += 1;
                    last_notified = at;
                }
            }
        }
        assert_eq!(reminders, 7);
    }

    #[test]
    fn an_alert_that_gets_worse_is_notified_at_once() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        // The certificate went from "30 days" to "7 days".
        let alert = [open(3, AlertKind::CertificateExpiring, "tak_server", Severity::Warning, 2)];
        let worse = evaluation(vec![finding(AlertKind::CertificateExpiring, "tak_server", Severity::Critical)]);
        let t = plan(&alert, &worse, &mut streaks, &policy, NOW);
        assert!(matches!(&t[..], [Transition::Escalate { id: 3, finding }] if finding.severity == Severity::Critical));
        // Getting better without being over is not news.
        let alert = [open(3, AlertKind::DiskFull, "root", Severity::Critical, 2)];
        let better = evaluation(vec![finding(AlertKind::DiskFull, "root", Severity::Warning)]);
        assert!(matches!(&plan(&alert, &better, &mut streaks, &policy, NOW)[..], [Transition::Refresh { .. }]));
    }

    #[test]
    fn an_alert_is_resolved_once_the_problem_is_gone_twice_in_a_row() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        let alert = [open(9, AlertKind::ServiceDown, "caddy", Severity::Critical, 1)];
        let fine = evaluation(vec![]);
        assert_eq!(plan(&alert, &fine, &mut streaks, &policy, NOW), vec![]);
        assert_eq!(plan(&alert, &fine, &mut streaks, &policy, NOW), vec![Transition::Resolve { id: 9 }]);
        // Gone once, back, gone once: still open.
        let mut streaks = Streaks::default();
        let down = evaluation(vec![finding(AlertKind::ServiceDown, "caddy", Severity::Critical)]);
        assert_eq!(plan(&alert, &fine, &mut streaks, &policy, NOW), vec![]);
        assert!(matches!(&plan(&alert, &down, &mut streaks, &policy, NOW)[..], [Transition::Refresh { .. }]));
        assert_eq!(plan(&alert, &fine, &mut streaks, &policy, NOW), vec![]);
    }

    #[test]
    fn an_alert_whose_source_is_unreadable_stays_as_it_is() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        // The disk was full; then the agent stopped answering. Nobody saw the disk get better.
        let alerts = [open(1, AlertKind::DiskFull, "root", Severity::Critical, 30), open(2, AlertKind::TakUnreachable, "", Severity::Critical, 1)];
        let blind = Evaluation {
            findings: vec![finding(AlertKind::AgentUnreachable, "", Severity::Warning)],
            judged: [AlertKind::AgentUnreachable, AlertKind::TakUnreachable].into(),
        };
        for _ in 0..5 {
            let transitions = plan(&alerts, &blind, &mut streaks, &policy, NOW);
            assert!(!transitions.iter().any(|t| matches!(t, Transition::Resolve { id: 1 } | Transition::Remind { id: 1, .. })), "{transitions:?}");
        }
        // TAK Server, which the platform probes itself, was judged and is fine: it resolved.
        let mut streaks = Streaks::default();
        plan(&alerts, &blind, &mut streaks, &policy, NOW);
        assert!(plan(&alerts, &blind, &mut streaks, &policy, NOW).contains(&Transition::Resolve { id: 2 }));
    }

    #[test]
    fn everything_failing_at_once_is_one_batch_of_transitions() {
        let policy = Policy::default();
        let mut streaks = Streaks::default();
        let mut obs = healthy();
        obs.services = Some(
            ["caddy", "app", "postgres", "openldap", "takserver", "takserver-db"]
                .iter()
                .map(|name| ServiceObservation { name: name.to_string(), state: Some("exited".into()), health: None, recent_restarts: 0 })
                .collect(),
        );
        obs.tak_failures = Some(vec!["8089".into(), "8443".into(), "8446".into(), "api".into()]);
        obs.directory_reachable = Some(false);
        let outage = evaluate(&obs, &Thresholds::default());
        assert_eq!(plan(&[], &outage, &mut streaks, &policy, NOW), vec![]);
        let raised = plan(&[], &outage, &mut streaks, &policy, NOW);
        assert_eq!(raised.len(), 8, "six services, TAK Server, the directory: all in the same evaluation");
        assert!(raised.iter().all(|t| matches!(t, Transition::Raise(_))));
    }

    #[test]
    fn a_policy_of_one_raises_and_resolves_at_once() {
        let policy = Policy { raise_after: 1, resolve_after: 1, ..Default::default() };
        let mut streaks = Streaks::default();
        let down = evaluation(vec![finding(AlertKind::BackupFailed, "", Severity::Warning)]);
        assert_eq!(plan(&[], &down, &mut streaks, &policy, NOW).len(), 1);
        let alert = [open(1, AlertKind::BackupFailed, "", Severity::Warning, 1)];
        assert_eq!(plan(&alert, &evaluation(vec![]), &mut streaks, &policy, NOW), vec![Transition::Resolve { id: 1 }]);
    }
}
