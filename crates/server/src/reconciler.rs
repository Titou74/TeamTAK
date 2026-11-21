//! The reconciliation service: reads the desired state from PostgreSQL, runs a pass of
//! `teamtak_core::reconcile`, and records what it found.
//!
//! It runs after every change made through the API (so the change is live when the request
//! returns), on a timer, at startup, on an administrator's request, and at the instant an
//! event starts or ends.
//!
//! The groups of an account depend on the time: its team's group, plus the group of every
//! running event its team takes part in. Each pass computes them for the time it runs at,
//! so an event needs no other mechanism than a pass at the right moment, and a pass run
//! late (the platform was stopped) still gives the right result.
//!
//! Video streams follow the same clock: a pass gives TAK Server the feeds of the streams
//! valid at its time, and disconnects from the video server whoever still uses a stream
//! that is not.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::PgPool;
use teamtak_core::directory::Directory;
use teamtak_core::events::{clock_now, groups_due, next_boundary, team_groups, EventGroup};
use teamtak_core::reconcile::{run_pass_with, Change, DesiredAccount, DesiredState, PassOptions, PassReport};
use teamtak_core::secret::SecretBox;
use teamtak_core::tak::{IssuedCertificate, TakServer};
use time::OffsetDateTime;
use tokio::sync::{Mutex, MutexGuard, Notify};
use uuid::Uuid;

use crate::video::{self, VideoServer};

/// Context of the password column, authenticated with the ciphertext.
pub const PASSWORD_CONTEXT: &str = "tak_accesses.password";

/// Accounts unknown to the database that an automatic pass may delete; beyond that it stops
/// and asks for a manual run (see `PassOptions::max_unknown_accounts`).
const AUTOMATIC_DELETION_LIMIT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Right after a change made through the API.
    Change,
    /// The timer.
    Periodic,
    /// An administrator asked for it.
    Manual,
    /// The server just started.
    Startup,
    /// An event starts or ends at this instant.
    Schedule,
}

impl Trigger {
    /// Every origin a pass can have; the UI has a text for each (`reconcile.triggers.<name>`).
    #[cfg_attr(not(test), allow(dead_code))]
    pub const ALL: &'static [Trigger] =
        &[Trigger::Change, Trigger::Periodic, Trigger::Manual, Trigger::Startup, Trigger::Schedule];

    pub fn as_str(self) -> &'static str {
        match self {
            Trigger::Change => "change",
            Trigger::Periodic => "periodic",
            Trigger::Manual => "manual",
            Trigger::Startup => "startup",
            Trigger::Schedule => "schedule",
        }
    }
}

pub struct Reconciler {
    db: PgPool,
    directory: Arc<dyn Directory>,
    tak: Arc<dyn TakServer>,
    secrets: SecretBox,
    protected_usernames: BTreeSet<String>,
    /// One pass at a time; also held by the API while it enrolls a certificate, so a pass
    /// never sees TAK Server and the database half-way through that.
    lock: Mutex<()>,
    /// Set when a pass that followed a change could not finish: what the next pass completes
    /// is then the end of that change, not drift.
    unfinished: AtomicBool,
    /// The time the latest pass computed the desired state for: every event boundary up to
    /// it has been given to a pass.
    last_clock: std::sync::Mutex<Option<OffsetDateTime>>,
    /// Wakes the scheduler when the dates of the events may have changed.
    schedule_changed: Notify,
    /// The video server; `None` when it is turned off, and TAK Server then lists none of
    /// the platform's video feeds.
    video: Option<Arc<VideoServer>>,
}

impl Reconciler {
    pub fn new(
        db: PgPool,
        directory: Arc<dyn Directory>,
        tak: Arc<dyn TakServer>,
        secrets: SecretBox,
        protected_usernames: BTreeSet<String>,
        video: Option<Arc<VideoServer>>,
    ) -> Self {
        Self {
            db,
            directory,
            tak,
            secrets,
            protected_usernames,
            lock: Mutex::new(()),
            unfinished: AtomicBool::new(false),
            last_clock: std::sync::Mutex::new(None),
            schedule_changed: Notify::new(),
            video,
        }
    }

    /// Tells the scheduler that an event was created, moved or closed.
    pub fn schedule_changed(&self) {
        self.schedule_changed.notify_one();
    }

    /// Events that are not cancelled and end after `after`, with the teams that take part:
    /// the host and every team that accepted.
    async fn events_ending_after(&self, after: OffsetDateTime) -> Result<Vec<EventGroup>> {
        let rows: Vec<(Uuid, String, OffsetDateTime, OffsetDateTime, Option<Uuid>)> = sqlx::query_as(
            "SELECT e.id, e.group_name, e.starts_at, e.ends_at, t.team_id
               FROM events e
               LEFT JOIN LATERAL (
                     SELECT e.host_team_id AS team_id WHERE e.host_team_id IS NOT NULL
                     UNION
                     SELECT p.team_id FROM event_teams p WHERE p.event_id = e.id AND p.status = 'accepted'
                    ) t ON true
              WHERE e.cancelled_at IS NULL AND e.ends_at > $1
              ORDER BY e.starts_at, e.id",
        )
        .bind(after)
        .fetch_all(&self.db)
        .await?;
        let mut events: BTreeMap<Uuid, EventGroup> = BTreeMap::new();
        for (id, group, starts_at, ends_at, team_id) in rows {
            let event = events.entry(id).or_insert_with(|| EventGroup {
                group,
                starts_at,
                ends_at,
                cancelled_at: None,
                teams: BTreeSet::new(),
            });
            event.teams.extend(team_id);
        }
        Ok(events.into_values().collect())
    }

    /// The next instant after `after` at which an event starts or ends.
    async fn next_boundary(&self, after: OffsetDateTime) -> Result<Option<OffsetDateTime>> {
        Ok(next_boundary(&self.events_ending_after(after).await?, after))
    }

    /// Holds reconciliation off while the caller changes TAK Server and the database together.
    pub async fn exclusive(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    /// What the database wants in the directory and in TAK Server at `now`, given the
    /// events that may be running then.
    pub async fn desired(&self, now: OffsetDateTime, events: &[EventGroup]) -> Result<DesiredState> {
        let rows: Vec<(String, String, Uuid, String, String)> = sqlx::query_as(
            "SELECT a.username, a.password_enc, t.id, t.slug, t.status
               FROM tak_accesses a JOIN teams t ON t.id = a.team_id
              ORDER BY a.username",
        )
        .fetch_all(&self.db)
        .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for (username, password_enc, team_id, slug, status) in rows {
            // An unreadable password aborts the pass: guessing here could delete accounts.
            let password = self
                .secrets
                .open_string(PASSWORD_CONTEXT, &password_enc)
                .with_context(|| format!("password of {username} (was APP_SECRET changed?)"))?;
            let password_tag = self.secrets.password_tag(&username, &password);
            // Only an active team exists in TAK Server; its accounts keep no group otherwise.
            // An active team is in its own group and in the group of each running event it
            // takes part in.
            let groups = team_groups(team_id, &slug, status == "active", events, now);
            accounts.push(DesiredAccount { username, password, password_tag, groups });
        }
        let revoke: Vec<(String,)> = sqlx::query_as(
            "SELECT fingerprint FROM tak_certificates WHERE revoke_requested_at IS NOT NULL AND inactive_at IS NULL",
        )
        .fetch_all(&self.db)
        .await?;
        // With the video server turned off, TAK Server must list none of its streams.
        let video_feeds = match &self.video {
            Some(server) => {
                let streams = video::valid_streams(&self.db, now).await?;
                video::desired_feeds(&streams, &server.endpoints, self.secrets.mac_key())
            }
            None => Vec::new(),
        };
        Ok(DesiredState {
            accounts,
            revoke_fingerprints: revoke.into_iter().map(|(f,)| f).collect(),
            protected_usernames: self.protected_usernames.clone(),
            video_feeds: Some(video_feeds),
        })
    }

    /// Runs a pass and records its outcome.
    pub async fn run(&self, trigger: Trigger) -> Result<PassReport> {
        let _guard = self.lock.lock().await;
        self.run_locked(trigger).await
    }

    /// Same as [`run`], for a caller that already holds [`exclusive`].
    pub async fn run_locked(&self, trigger: Trigger) -> Result<PassReport> {
        let outcome = self.pass(trigger).await;
        // The dates of the events may have changed, or this pass applied a boundary the
        // scheduler was waiting for: let it look again.
        self.schedule_changed.notify_one();
        outcome
    }

    /// The time the latest pass without error computed the desired state for.
    async fn last_desired_at(&self) -> Result<Option<OffsetDateTime>> {
        let (at,): (Option<OffsetDateTime>,) =
            sqlx::query_as("SELECT desired_at FROM reconcile_status WHERE id = 1").fetch_one(&self.db).await?;
        Ok(at)
    }

    async fn pass(&self, trigger: Trigger) -> Result<PassReport> {
        // One clock reading for the whole pass: the groups are those of that instant.
        let now = clock_now();
        *self.last_clock.lock().unwrap() = Some(now);
        let read = async {
            let since = self.last_desired_at().await?;
            let events = self.events_ending_after(since.map_or(now, |s| s.min(now))).await?;
            let desired = self.desired(now, &events).await?;
            // Groups of the events that started or ended since the last complete pass: this
            // pass is the one that applies them, whatever triggered it.
            let due = since.map(|s| groups_due(&events, s, now)).unwrap_or_default();
            anyhow::Ok((desired, due))
        };
        let (desired, due) = match read.await {
            Ok(d) => d,
            Err(e) => {
                let message = format!("reading the desired state: {e:#}");
                self.record_failure(&message).await;
                return Err(e);
            }
        };
        let finishing_a_change = self.unfinished.load(Ordering::SeqCst);
        let automatic = matches!(trigger, Trigger::Periodic | Trigger::Startup | Trigger::Schedule) && !finishing_a_change;
        let options = PassOptions { max_unknown_accounts: automatic.then_some(AUTOMATIC_DELETION_LIMIT) };
        let mut report = run_pass_with(&desired, self.directory.as_ref(), self.tak.as_ref(), options).await;
        if let Some(server) = &self.video {
            // The video server is not a backend of the reconciliation: it being down is a
            // service down (monitoring), not a synchronisation failing.
            match video::valid_streams(&self.db, now).await {
                Ok(valid) => report.changes.extend(video::stop_invalid(server, &valid).await),
                Err(e) => tracing::warn!(error = %e, "video: valid streams not read"),
            }
        }

        // A change found by a pass nobody asked for is drift, unless it is the schedule of
        // an event at work: a membership of a group that was due to appear or disappear.
        let expected = matches!(trigger, Trigger::Change) || finishing_a_change;
        if expected {
            self.unfinished.store(!report.errors.is_empty(), Ordering::SeqCst);
        }
        let drift: Vec<bool> = report.changes.iter().map(|change| !expected && !is_scheduled(change, &due)).collect();
        for (change, is_drift) in report.changes.iter().zip(&drift) {
            tracing::info!(
                trigger = trigger.as_str(),
                kind = change.kind(),
                subject = change.subject(),
                group = change.group(),
                drift = is_drift,
                "reconciliation change"
            );
        }
        for error in &report.errors {
            tracing::warn!(trigger = trigger.as_str(), error, "reconciliation error");
        }
        if let Err(e) = self.persist(trigger, now, &drift, &report).await {
            tracing::error!(error = %format!("{e:#}"), "recording the reconciliation outcome");
        }
        Ok(report)
    }

    async fn record_failure(&self, message: &str) {
        tracing::error!(error = message, "reconciliation aborted");
        let _ = sqlx::query("UPDATE reconcile_status SET last_run_at = now(), last_errors = $1 WHERE id = 1")
            .bind(serde_json::json!([message]))
            .execute(&self.db)
            .await;
    }

    async fn persist(&self, trigger: Trigger, now: OffsetDateTime, drift: &[bool], report: &PassReport) -> Result<()> {
        let mut tx = self.db.begin().await?;
        for (change, is_drift) in report.changes.iter().zip(drift) {
            sqlx::query(
                "INSERT INTO reconcile_events (trigger, kind, subject, group_name, is_drift) VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(trigger.as_str())
            .bind(change.kind())
            .bind(change.subject())
            .bind(change.group())
            .bind(is_drift)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "UPDATE reconcile_status
                SET last_run_at = now(),
                    last_ok_at = CASE WHEN $1 THEN now() ELSE last_ok_at END,
                    desired_at = CASE WHEN $1 THEN $5 ELSE desired_at END,
                    directory_reachable = $2, tak_reachable = $3, last_errors = $4
              WHERE id = 1",
        )
        .bind(report.errors.is_empty())
        .bind(report.directory_reachable)
        .bind(report.tak_reachable)
        .bind(serde_json::json!(report.errors))
        .bind(now)
        .execute(&mut *tx)
        .await?;
        if let Some(active) = &report.active_certificates {
            sync_certificates(&mut tx, active).await?;
        }
        // Keep the journal bounded.
        sqlx::query("DELETE FROM reconcile_events WHERE occurred_at < now() - interval '90 days'").execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

/// Brings the `tak_certificates` table in line with the certificates TAK Server lists as
/// active: new ones (enrolled by devices) are recorded with their expiry, and those no
/// longer listed are marked inactive.
pub async fn sync_certificates(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, active: &[IssuedCertificate]) -> Result<()> {
    let accesses: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, username FROM tak_accesses").fetch_all(&mut **tx).await?;
    let by_username: BTreeMap<String, Uuid> = accesses.into_iter().map(|(id, username)| (username, id)).collect();
    for cert in active {
        let Some(access_id) = by_username.get(&cert.username) else { continue };
        sqlx::query(
            "INSERT INTO tak_certificates (id, access_id, username, fingerprint, source, issued_at, expires_at)
             VALUES ($1, $2, $3, $4, 'device', $5, $6)
             ON CONFLICT (fingerprint) DO UPDATE
                SET access_id = EXCLUDED.access_id,
                    issued_at = COALESCE(EXCLUDED.issued_at, tak_certificates.issued_at),
                    expires_at = COALESCE(EXCLUDED.expires_at, tak_certificates.expires_at),
                    inactive_at = NULL",
        )
        .bind(Uuid::new_v4())
        .bind(access_id)
        .bind(&cert.username)
        .bind(&cert.fingerprint)
        .bind(cert.issued_at)
        .bind(cert.expires_at)
        .execute(&mut **tx)
        .await?;
    }
    let fingerprints: Vec<String> = active.iter().map(|c| c.fingerprint.clone()).collect();
    // The key material of a certificate that no longer works is of no use: drop it.
    sqlx::query(
        "UPDATE tak_certificates
            SET inactive_at = now(), p12_enc = NULL, p12_password_enc = NULL
          WHERE inactive_at IS NULL AND NOT (fingerprint = ANY($1))",
    )
    .bind(&fingerprints)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// True when a change is what the start or the end of an event calls for: an account
/// entering or leaving a group that was due, or a video stream of that group appearing in
/// TAK Server or leaving it.
fn is_scheduled(change: &Change, due: &BTreeSet<String>) -> bool {
    let follows_the_group = matches!(
        change,
        Change::MemberAdded { .. }
            | Change::MemberRemoved { .. }
            | Change::VideoFeedAdded { .. }
            | Change::VideoFeedRemoved { .. }
            | Change::VideoStopped { .. }
    );
    follows_the_group && change.group().is_some_and(|g| due.contains(g))
}

/// Runs a pass at startup, then every `interval`, and at each instant an event starts or
/// ends, for ever.
///
/// The periodic pass alone would apply an event up to one period late. The scheduler sleeps
/// until the next start or end instead; it is woken when the dates change, and looks again
/// after every pass. If that pass fails (TAK Server down), the periodic one retries.
pub async fn run_periodically(reconciler: Arc<Reconciler>, interval: std::time::Duration) {
    if let Err(e) = reconciler.run(Trigger::Startup).await {
        tracing::error!(error = %format!("{e:#}"), "startup reconciliation failed");
    }
    let mut next_tick = tokio::time::Instant::now() + interval;
    loop {
        // Boundaries up to the latest pass's clock were given to that pass, successful or not.
        let handled = reconciler.last_clock.lock().unwrap().unwrap_or_else(clock_now);
        let boundary = match reconciler.next_boundary(handled).await {
            Ok(boundary) => boundary,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "reading the next event boundary");
                None
            }
        };
        let until_boundary = boundary.map(|at| std::time::Duration::try_from(at - clock_now()).unwrap_or_default());
        let trigger = tokio::select! {
            _ = tokio::time::sleep_until(next_tick) => Trigger::Periodic,
            _ = tokio::time::sleep(until_boundary.unwrap_or_default()), if until_boundary.is_some() => Trigger::Schedule,
            _ = reconciler.schedule_changed.notified() => continue,
        };
        match trigger {
            Trigger::Periodic => next_tick = tokio::time::Instant::now() + interval,
            _ => {
                // The pass must not read the clock a hair before the boundary it is run for.
                while boundary.is_some_and(|at| clock_now() < at) {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
                tracing::info!(boundary = ?boundary, "an event starts or ends");
            }
        }
        if let Err(e) = reconciler.run(trigger).await {
            tracing::error!(trigger = trigger.as_str(), error = %format!("{e:#}"), "reconciliation failed");
        }
        // That pass notified the scheduler itself; it is already looking again.
        let _ = tokio::time::timeout(std::time::Duration::ZERO, reconciler.schedule_changed.notified()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_memberships_of_due_groups_are_the_schedule_at_work() {
        let due: BTreeSet<String> = ["evt-exercise".to_string()].into();
        let added = |group: &str| Change::MemberAdded { group: group.into(), username: "alpha-shared".into() };
        let removed = |group: &str| Change::MemberRemoved { group: group.into(), username: "alpha-shared".into() };
        assert!(is_scheduled(&added("evt-exercise"), &due));
        assert!(is_scheduled(&removed("evt-exercise"), &due));
        // Anything else found by the same pass is still drift.
        assert!(!is_scheduled(&added("evt-other"), &due), "an event that was not due");
        assert!(!is_scheduled(&added("alpha"), &due), "a team group");
        assert!(!is_scheduled(&Change::UserDeleted { username: "alpha-shared".into() }, &due));
        assert!(!is_scheduled(&Change::ForeignMemberRemoved { group: "evt-exercise".into(), member: "x".into() }, &due));
        assert!(!is_scheduled(&added("evt-exercise"), &BTreeSet::new()), "nothing was due");
        let feed = Change::VideoFeedAdded { path: "evt-exercise/cam".into(), group: "evt-exercise".into() };
        assert!(is_scheduled(&feed, &due), "an event's stream appears when the event starts");
        let stopped = Change::VideoStopped { path: "evt-exercise/cam".into(), group: "evt-exercise".into() };
        assert!(is_scheduled(&stopped, &due), "and is cut when it ends");
        let updated = Change::VideoFeedUpdated { path: "evt-exercise/cam".into(), group: "evt-exercise".into() };
        assert!(!is_scheduled(&updated, &due), "an address changed by nobody is drift");
    }

    #[test]
    fn every_trigger_has_a_distinct_name() {
        let names: BTreeSet<&str> = Trigger::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(names.len(), 5);
    }
}
