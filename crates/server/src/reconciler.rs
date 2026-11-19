//! The reconciliation service: reads the desired state from PostgreSQL, runs a pass of
//! `tak_manager_core::reconcile`, and records what it found.
//!
//! It runs after every change made through the API (so the change is live when the request
//! returns), on a timer, at startup, and on an administrator's request.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tak_manager_core::directory::Directory;
use tak_manager_core::reconcile::{run_pass_with, DesiredAccount, DesiredState, PassOptions, PassReport};
use tak_manager_core::secret::SecretBox;
use tak_manager_core::tak::{IssuedCertificate, TakServer};
use tokio::sync::{Mutex, MutexGuard};
use uuid::Uuid;

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
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Trigger::Change => "change",
            Trigger::Periodic => "periodic",
            Trigger::Manual => "manual",
            Trigger::Startup => "startup",
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
}

impl Reconciler {
    pub fn new(
        db: PgPool,
        directory: Arc<dyn Directory>,
        tak: Arc<dyn TakServer>,
        secrets: SecretBox,
        protected_usernames: BTreeSet<String>,
    ) -> Self {
        Self { db, directory, tak, secrets, protected_usernames, lock: Mutex::new(()), unfinished: AtomicBool::new(false) }
    }

    /// Holds reconciliation off while the caller changes TAK Server and the database together.
    pub async fn exclusive(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().await
    }

    /// What the database wants in the directory and in TAK Server.
    pub async fn desired(&self) -> Result<DesiredState> {
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(
            "SELECT a.username, a.password_enc, t.slug, t.status
               FROM tak_accesses a JOIN teams t ON t.id = a.team_id
              ORDER BY a.username",
        )
        .fetch_all(&self.db)
        .await?;
        let mut accounts = Vec::with_capacity(rows.len());
        for (username, password_enc, slug, status) in rows {
            // An unreadable password aborts the pass: guessing here could delete accounts.
            let password = self
                .secrets
                .open_string(PASSWORD_CONTEXT, &password_enc)
                .with_context(|| format!("password of {username} (was APP_SECRET changed?)"))?;
            let password_tag = self.secrets.password_tag(&username, &password);
            // Only an active team exists in TAK Server; its accounts keep no group otherwise.
            let groups = if status == "active" { BTreeSet::from([slug]) } else { BTreeSet::new() };
            accounts.push(DesiredAccount { username, password, password_tag, groups });
        }
        let revoke: Vec<(String,)> = sqlx::query_as(
            "SELECT fingerprint FROM tak_certificates WHERE revoke_requested_at IS NOT NULL AND inactive_at IS NULL",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(DesiredState {
            accounts,
            revoke_fingerprints: revoke.into_iter().map(|(f,)| f).collect(),
            protected_usernames: self.protected_usernames.clone(),
        })
    }

    /// Runs a pass and records its outcome.
    pub async fn run(&self, trigger: Trigger) -> Result<PassReport> {
        let _guard = self.lock.lock().await;
        self.run_locked(trigger).await
    }

    /// Same as [`run`], for a caller that already holds [`exclusive`].
    pub async fn run_locked(&self, trigger: Trigger) -> Result<PassReport> {
        let desired = match self.desired().await {
            Ok(d) => d,
            Err(e) => {
                let message = format!("reading the desired state: {e:#}");
                self.record_failure(&message).await;
                return Err(e);
            }
        };
        let finishing_a_change = self.unfinished.load(Ordering::SeqCst);
        let automatic = matches!(trigger, Trigger::Periodic | Trigger::Startup) && !finishing_a_change;
        let options = PassOptions { max_unknown_accounts: automatic.then_some(AUTOMATIC_DELETION_LIMIT) };
        let report = run_pass_with(&desired, self.directory.as_ref(), self.tak.as_ref(), options).await;

        // A change found by a pass nobody asked for is drift.
        let is_drift = !matches!(trigger, Trigger::Change) && !finishing_a_change;
        if matches!(trigger, Trigger::Change) || finishing_a_change {
            self.unfinished.store(!report.errors.is_empty(), Ordering::SeqCst);
        }
        for change in &report.changes {
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
        if let Err(e) = self.persist(trigger, is_drift, &report).await {
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

    async fn persist(&self, trigger: Trigger, is_drift: bool, report: &PassReport) -> Result<()> {
        let mut tx = self.db.begin().await?;
        for change in &report.changes {
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
                    directory_reachable = $2, tak_reachable = $3, last_errors = $4
              WHERE id = 1",
        )
        .bind(report.errors.is_empty())
        .bind(report.directory_reachable)
        .bind(report.tak_reachable)
        .bind(serde_json::json!(report.errors))
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

/// Runs a pass every `interval`, for ever.
pub async fn run_periodically(reconciler: Arc<Reconciler>, interval: std::time::Duration) {
    if let Err(e) = reconciler.run(Trigger::Startup).await {
        tracing::error!(error = %format!("{e:#}"), "startup reconciliation failed");
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        if let Err(e) = reconciler.run(Trigger::Periodic).await {
            tracing::error!(error = %format!("{e:#}"), "periodic reconciliation failed");
        }
    }
}
