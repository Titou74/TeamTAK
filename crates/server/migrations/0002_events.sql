-- TeamTAK, phase 2: cross-team events.
--
-- An event is a period during which the TAK accounts of its host team and of every team
-- that accepted its invitation share one more group. Its state is not stored: it follows
-- from starts_at, ends_at and cancelled_at and the time it is, so an event starts and ends
-- without any write. The reconciliation gives the group to the accounts while it runs.

CREATE TABLE events (
    id             uuid PRIMARY KEY,
    name           text NOT NULL,
    -- Name of the TAK group: cn of the LDAP group, channel name in ATAK. 'evt-' followed by
    -- the slug of the name. Unique among events that are not over (checked on creation: it
    -- depends on the time, so no index can hold it).
    group_name     text NOT NULL,
    -- The team that created the event. Deleting it cancels the event; the row stays so the
    -- other teams still see what happened.
    host_team_id   uuid REFERENCES teams (id) ON DELETE SET NULL,
    host_team_name text NOT NULL,
    -- UTC. The start is inclusive, the end exclusive.
    starts_at      timestamptz NOT NULL,
    ends_at        timestamptz NOT NULL,
    cancelled_at   timestamptz,
    -- Who called it off: a leader of the host team, an administrator, or the deletion of
    -- the host team.
    cancel_reason  text CHECK (cancel_reason IN ('host', 'admin', 'host_deleted')),
    created_by     uuid REFERENCES accounts (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at >= starts_at),
    CHECK ((cancelled_at IS NULL) = (cancel_reason IS NULL))
);
CREATE INDEX events_host_idx ON events (host_team_id);
CREATE INDEX events_group_idx ON events (group_name) WHERE cancelled_at IS NULL;
CREATE INDEX events_end_idx ON events (ends_at) WHERE cancelled_at IS NULL;

-- The teams invited to an event. The host team is not listed: it takes part by definition.
CREATE TABLE event_teams (
    event_id    uuid NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    team_id     uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    -- invited: waiting for an answer; accepted: takes part; declined; left: had accepted,
    -- then walked out; removed: taken out by the host.
    status      text NOT NULL CHECK (status IN ('invited', 'accepted', 'declined', 'left', 'removed')),
    invited_by  uuid REFERENCES accounts (id) ON DELETE SET NULL,
    invited_at  timestamptz NOT NULL DEFAULT now(),
    answered_by uuid REFERENCES accounts (id) ON DELETE SET NULL,
    answered_at timestamptz,
    PRIMARY KEY (event_id, team_id)
);
CREATE INDEX event_teams_team_idx ON event_teams (team_id);

-- A pass run because an event starts or ends has its own origin in the journal.
ALTER TABLE reconcile_events DROP CONSTRAINT reconcile_events_trigger_check;
ALTER TABLE reconcile_events
    ADD CONSTRAINT reconcile_events_trigger_check CHECK (trigger IN ('change', 'periodic', 'manual', 'startup', 'schedule'));

-- The time for which the last pass without error computed the desired state. An event that
-- started or ended after it has not been applied yet: what the next pass changes for it is
-- the schedule at work, not drift, even when the platform was stopped in between.
ALTER TABLE reconcile_status ADD COLUMN desired_at timestamptz;
