-- TeamTAK, phase 1: accounts, teams, TAK accesses, reconciliation.
-- This database is the source of truth; OpenLDAP and TAK Server are made to match it.

CREATE TABLE accounts (
    id            uuid PRIMARY KEY,
    -- Lowercase; the login code proves the address works.
    email         text NOT NULL UNIQUE,
    display_name  text NOT NULL DEFAULT '',
    -- Language of the UI and of the e-mails sent to this account.
    language      text NOT NULL DEFAULT 'en',
    is_admin      boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz
);

-- One-time login codes. Rows also serve the rate limits (per address, per IP).
CREATE TABLE login_codes (
    id          uuid PRIMARY KEY,
    email       text NOT NULL,
    -- HMAC of the address and the code; the code itself is never stored.
    code_hash   text NOT NULL,
    ip          text NOT NULL,
    attempts    integer NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL,
    consumed_at timestamptz
);
CREATE INDEX login_codes_email_idx ON login_codes (email, created_at);
CREATE INDEX login_codes_ip_idx ON login_codes (ip, created_at);

-- Wrong codes submitted, for the per-IP limit on guesses.
CREATE TABLE login_failures (
    id         bigserial PRIMARY KEY,
    ip         text NOT NULL,
    email      text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX login_failures_ip_idx ON login_failures (ip, created_at);

CREATE TABLE sessions (
    id           uuid PRIMARY KEY,
    -- SHA-256 of the cookie value.
    token_hash   text NOT NULL UNIQUE,
    account_id   uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL
);

-- A team is one TAK group. It starts as a request (pending) and exists in TAK Server only
-- while it is active.
CREATE TABLE teams (
    id              uuid PRIMARY KEY,
    name            text NOT NULL,
    -- Name of the TAK group: cn of the LDAP group, channel name in ATAK.
    slug            text NOT NULL,
    status          text NOT NULL CHECK (status IN ('pending', 'active', 'rejected', 'suspended')),
    -- How members connect to TAK Server.
    mode            text NOT NULL CHECK (mode IN ('shared', 'individual', 'external')),
    -- Members plus pending invitations may not exceed it.
    member_cap      integer NOT NULL CHECK (member_cap >= 1),
    request_message text NOT NULL DEFAULT '',
    requested_by    uuid NOT NULL REFERENCES accounts (id),
    decided_by      uuid REFERENCES accounts (id),
    decided_at      timestamptz,
    decision_note   text NOT NULL DEFAULT '',
    created_at      timestamptz NOT NULL DEFAULT now()
);
-- A rejected request frees its name.
CREATE UNIQUE INDEX teams_slug_idx ON teams (slug) WHERE status <> 'rejected';

CREATE TABLE team_members (
    team_id    uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    account_id uuid NOT NULL REFERENCES accounts (id) ON DELETE CASCADE,
    role       text NOT NULL CHECK (role IN ('leader', 'member')),
    joined_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (team_id, account_id)
);
CREATE INDEX team_members_account_idx ON team_members (account_id);

-- Invitations are addressed to an e-mail: the invitee may not have an account yet.
CREATE TABLE invitations (
    id         uuid PRIMARY KEY,
    team_id    uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    email      text NOT NULL,
    invited_by uuid NOT NULL REFERENCES accounts (id),
    status     text NOT NULL CHECK (status IN ('pending', 'accepted', 'declined', 'cancelled')),
    created_at timestamptz NOT NULL DEFAULT now(),
    decided_at timestamptz
);
CREATE UNIQUE INDEX invitations_pending_idx ON invitations (team_id, email) WHERE status = 'pending';
CREATE INDEX invitations_email_idx ON invitations (email) WHERE status = 'pending';

-- A TAK access is one account in the directory, member of its team's group.
CREATE TABLE tak_accesses (
    id             uuid PRIMARY KEY,
    team_id        uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    kind           text NOT NULL CHECK (kind IN ('shared', 'individual', 'external')),
    -- The member it belongs to (individual accesses only).
    account_id     uuid REFERENCES accounts (id) ON DELETE CASCADE,
    -- Who it is for (external accesses only).
    label          text NOT NULL DEFAULT '',
    -- LDAP uid and CN of the certificates.
    username       text NOT NULL UNIQUE,
    -- Encrypted with the application key: the platform shows it again (QR code, manual entry).
    password_enc   text NOT NULL,
    -- Raised by each "regenerate".
    generation     integer NOT NULL DEFAULT 1,
    created_by     uuid REFERENCES accounts (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    regenerated_at timestamptz,
    CHECK ((kind = 'individual') = (account_id IS NOT NULL))
);
CREATE UNIQUE INDEX tak_accesses_shared_idx ON tak_accesses (team_id) WHERE kind = 'shared';
CREATE UNIQUE INDEX tak_accesses_individual_idx ON tak_accesses (team_id, account_id) WHERE kind = 'individual';

-- Usernames of removed accesses are never given out again: a certificate issued to the old
-- holder must not match a new account.
CREATE TABLE retired_usernames (
    username   text PRIMARY KEY,
    retired_at timestamptz NOT NULL DEFAULT now()
);

-- Client certificates issued by TAK Server for the accesses: those the platform enrolled
-- itself to ship in a zip ('package') and those devices enrolled by themselves ('device',
-- discovered by the reconciliation). expires_at is what monitoring alerts on.
CREATE TABLE tak_certificates (
    id                  uuid PRIMARY KEY,
    access_id           uuid REFERENCES tak_accesses (id) ON DELETE SET NULL,
    username            text NOT NULL,
    -- SHA-256 of the certificate, lowercase hexadecimal.
    fingerprint         text NOT NULL UNIQUE,
    source              text NOT NULL CHECK (source IN ('package', 'device')),
    issued_at           timestamptz,
    expires_at          timestamptz,
    first_seen_at       timestamptz NOT NULL DEFAULT now(),
    -- Set by "regenerate": the reconciliation revokes it in TAK Server.
    revoke_requested_at timestamptz,
    -- Set once TAK Server no longer lists it as active (revoked or expired).
    inactive_at         timestamptz,
    -- 'package' only: the PKCS#12 store and its password, encrypted with the application key.
    p12_enc             text,
    p12_password_enc    text
);
CREATE INDEX tak_certificates_access_idx ON tak_certificates (access_id);
CREATE INDEX tak_certificates_expiry_idx ON tak_certificates (expires_at) WHERE inactive_at IS NULL;

-- State of the reconciliation loop (single row).
CREATE TABLE reconcile_status (
    id                  integer PRIMARY KEY CHECK (id = 1),
    last_run_at         timestamptz,
    -- Last pass that ended without any error.
    last_ok_at          timestamptz,
    directory_reachable boolean NOT NULL DEFAULT false,
    tak_reachable       boolean NOT NULL DEFAULT false,
    last_errors         jsonb NOT NULL DEFAULT '[]'
);
INSERT INTO reconcile_status (id) VALUES (1);

-- What reconciliation passes changed. is_drift marks a change found by a periodic pass, that
-- is one the platform did not just cause itself.
CREATE TABLE reconcile_events (
    id          bigserial PRIMARY KEY,
    occurred_at timestamptz NOT NULL DEFAULT now(),
    trigger     text NOT NULL CHECK (trigger IN ('change', 'periodic', 'manual', 'startup')),
    kind        text NOT NULL,
    subject     text NOT NULL,
    group_name  text,
    is_drift    boolean NOT NULL
);
CREATE INDEX reconcile_events_time_idx ON reconcile_events (occurred_at DESC);
