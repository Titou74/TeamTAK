-- TeamTAK, phase 3: audit log, alerts, monitoring history, settings, backups.
--
-- Every table added here that grows with time is bounded by design: the platform purges it
-- once a day (crates/server/src/monitor/maintenance.rs), with the retention written next
-- to each table.

-- Who did what, when, on which team or object, from which address. One row per
-- state-changing action, administrator sign-in, use of the rescue command, and action of
-- the host agent. Never a secret: `details` only holds the values listed in
-- crates/server/src/audit.rs. Kept `audit_retention_days` (settings), one year by default.
CREATE TABLE audit_log (
    id          bigserial PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    -- No foreign key: the entry outlives the account, and the address is kept as it was.
    actor_id    uuid,
    -- Empty for what the platform does by itself and for the server's command line.
    actor_email text NOT NULL DEFAULT '',
    -- Stable code, translated by the UI (audit.actions.<code>).
    action      text NOT NULL,
    team_id     uuid,
    -- The team's name at that time: the team may be renamed or deleted since.
    team_name   text NOT NULL DEFAULT '',
    -- What the action was on: a TAK username, an e-mail address, an event, a service.
    object      text NOT NULL DEFAULT '',
    details     jsonb NOT NULL DEFAULT '{}',
    ip          text NOT NULL DEFAULT ''
);
CREATE INDEX audit_log_time_idx ON audit_log (occurred_at DESC, id DESC);
CREATE INDEX audit_log_team_idx ON audit_log (team_id, occurred_at DESC);
CREATE INDEX audit_log_action_idx ON audit_log (action, occurred_at DESC);
CREATE INDEX audit_log_actor_idx ON audit_log (actor_email, occurred_at DESC);

-- Alerts. Open while resolved_at is null; at most one open alert per kind and subject.
-- Resolved alerts are kept 90 days.
CREATE TABLE alerts (
    id               bigserial PRIMARY KEY,
    -- Stable code (crates/core/src/alerts.rs), translated by the UI and the e-mails.
    kind             text NOT NULL,
    -- What it is about when a kind can have several at once: a service, a filesystem.
    subject          text NOT NULL DEFAULT '',
    severity         text NOT NULL CHECK (severity IN ('low', 'warning', 'critical')),
    -- Figures the texts are filled with.
    details          jsonb NOT NULL DEFAULT '{}',
    raised_at        timestamptz NOT NULL DEFAULT now(),
    last_seen_at     timestamptz NOT NULL DEFAULT now(),
    -- Last time the administrators were told (raised, got worse, reminder).
    last_notified_at timestamptz,
    resolved_at      timestamptz
);
CREATE UNIQUE INDEX alerts_open_idx ON alerts (kind, subject) WHERE resolved_at IS NULL;
CREATE INDEX alerts_time_idx ON alerts (raised_at DESC);

-- A modest history for the dashboard's charts: one row every five minutes, kept 14 days
-- (about 4,000 rows).
CREATE TABLE metric_samples (
    sampled_at     timestamptz PRIMARY KEY,
    cpu_percent    real,
    load1          real,
    memory_percent real,
    swap_percent   real,
    -- The fullest filesystem.
    disk_percent   real,
    -- Devices connected to TAK Server.
    devices        integer
);

-- Settings an administrator changes from the web UI (single row). `data` holds thresholds,
-- the backup schedule, the webhook address, retention periods: see
-- crates/server/src/settings.rs for the fields and their defaults.
CREATE TABLE settings (
    id                    integer PRIMARY KEY CHECK (id = 1),
    data                  jsonb NOT NULL DEFAULT '{}',
    -- Passphrase of the backup archives, encrypted with the application key. Never returned
    -- by the API.
    backup_passphrase_enc text,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    updated_by            text NOT NULL DEFAULT ''
);
INSERT INTO settings (id) VALUES (1);

-- Backups asked of the host agent, by the daily schedule or by an administrator. Kept 90
-- days; the archives themselves follow their own retention (settings).
CREATE TABLE backup_runs (
    id           bigserial PRIMARY KEY,
    started_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz,
    trigger      text NOT NULL CHECK (trigger IN ('schedule', 'manual')),
    requested_by text NOT NULL DEFAULT '',
    ok           boolean,
    file         text,
    size_bytes   bigint,
    encrypted    boolean NOT NULL DEFAULT false,
    -- Last lines of the backup script when it failed.
    error        text
);
CREATE INDEX backup_runs_time_idx ON backup_runs (started_at DESC);

-- E-mails the mail server did not take. No recipient is stored. Kept 30 days.
CREATE TABLE mail_failures (
    id          bigserial PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    template    text NOT NULL,
    error       text NOT NULL
);
CREATE INDEX mail_failures_time_idx ON mail_failures (occurred_at DESC);

-- What the monitoring remembers between two runs and across restarts (single row).
CREATE TABLE monitor_state (
    id                   integer PRIMARY KEY CHECK (id = 1),
    last_mail_ok_at      timestamptz,
    last_mail_failure_at timestamptz,
    -- The day the daily jobs (backup, housekeeping, purges) last ran, and when.
    daily_ran_on         date,
    daily_ran_at         timestamptz,
    -- What the daily jobs did, for the dashboard.
    daily_report         jsonb NOT NULL DEFAULT '{}',
    -- Registry digests of the images pulled by tag, and when they were asked: registries are
    -- asked at most twice a day.
    image_checks         jsonb NOT NULL DEFAULT '{}',
    image_checked_at     timestamptz
);
INSERT INTO monitor_state (id) VALUES (1);

-- Notices sent to team leaders about a certificate shipped in one of their zips nearing its
-- end: one per certificate and threshold (30 and 7 days by default), never twice.
CREATE TABLE certificate_notices (
    fingerprint    text NOT NULL,
    threshold_days integer NOT NULL,
    sent_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (fingerprint, threshold_days)
);
