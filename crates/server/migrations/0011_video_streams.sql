-- TeamTAK: video streams on the video server (MediaMTX).
--
-- A team declares a stream for its own channel or for the channel of an event it takes part
-- in. Its path on the video server is <group_name>/<name>. Its two keys, to publish and to
-- watch, are not stored: they are derived from the application key, the stream's id and its
-- generation. Whether it is valid follows from its team and its event at the time it is,
-- like the groups of the team's accounts: nothing is written when an event starts or ends.

CREATE TABLE video_streams (
    id             uuid PRIMARY KEY,
    team_id        uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    -- The event whose channel it belongs to; NULL for the team's own channel.
    event_id       uuid REFERENCES events (id) ON DELETE CASCADE,
    -- The TAK group: the team's slug, or the event's group name. Copied, so the path of a
    -- stream never moves.
    group_name     text NOT NULL,
    -- Last segment of the path, from the label.
    name           text NOT NULL,
    -- What ATAK shows, followed by the team's name.
    label          text NOT NULL,
    -- Raised by each "regenerate": both keys change.
    generation     integer NOT NULL DEFAULT 1,
    -- A member manages the streams they declared.
    created_by     uuid REFERENCES accounts (id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    regenerated_at timestamptz
);
-- One stream per path. Two events can have had the same group, one after the other: their
-- streams are told apart by the event.
CREATE UNIQUE INDEX video_streams_team_path_idx ON video_streams (group_name, name) WHERE event_id IS NULL;
CREATE UNIQUE INDEX video_streams_event_path_idx ON video_streams (event_id, name) WHERE event_id IS NOT NULL;
CREATE INDEX video_streams_team_idx ON video_streams (team_id);
CREATE INDEX video_streams_path_idx ON video_streams (group_name, name);
