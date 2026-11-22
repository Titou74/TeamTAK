-- TeamTAK: map sources for ATAK, handed to the devices of a team or of an event through TAK
-- Server's device profiles.
--
-- The administrators keep the catalogue; a team's leaders add sources of their own, which
-- only their team sees. A team's leaders pick the team's maps, and an event's organisers the
-- event's maps. Whether an event's maps reach its devices follows from the clock, like its
-- group: nothing is written when an event starts or ends.
--
-- Every source is checked before it is stored (one of its tiles must be an image), and again
-- regularly: one that fails twice in a row is held back from the devices until it works.

CREATE TABLE map_sources (
    id         uuid PRIMARY KEY,
    -- Name of the file on the devices (IGN-Plan.xml). A device keeps a map source by its file
    -- name, so it never changes: uploading a file of the same name replaces the content.
    file_name  text NOT NULL,
    -- The name ATAK lists, read from the file.
    name       text NOT NULL,
    -- The file as uploaded, once checked: a customMapSource, no DTD.
    content    text NOT NULL,
    -- The tile URL template and its host, read from the file.
    url        text NOT NULL,
    url_host   text NOT NULL,
    min_zoom   smallint,
    max_zoom   smallint,
    tile_type  text,
    -- The team that added it, which alone sees it; NULL for the administrators' catalogue.
    team_id    uuid REFERENCES teams (id) ON DELETE CASCADE,
    -- The latest check: when, failures in a row, and why the last one failed
    -- (teamtak_core::maps::CheckError codes, with the HTTP status for http_status).
    checked_at     timestamptz NOT NULL DEFAULT now(),
    check_failures integer NOT NULL DEFAULT 0,
    check_error    text,
    check_status   integer,
    -- Held back from the devices: it failed twice in a row.
    broken     boolean NOT NULL DEFAULT false,
    created_by uuid REFERENCES accounts (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);
-- One file name for one source, whoever added it: a device keeps a map source by its name.
CREATE UNIQUE INDEX map_sources_file_name_idx ON map_sources (lower(file_name));
CREATE INDEX map_sources_team_idx ON map_sources (team_id);

-- The maps of a team's channel.
CREATE TABLE team_maps (
    team_id   uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    source_id uuid NOT NULL REFERENCES map_sources (id) ON DELETE CASCADE,
    added_by  uuid REFERENCES accounts (id) ON DELETE SET NULL,
    added_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (team_id, source_id)
);
CREATE INDEX team_maps_source_idx ON team_maps (source_id);

-- The maps of an event's channel, for the devices of every team that takes part, while it
-- runs.
CREATE TABLE event_maps (
    event_id  uuid NOT NULL REFERENCES events (id) ON DELETE CASCADE,
    source_id uuid NOT NULL REFERENCES map_sources (id) ON DELETE CASCADE,
    added_by  uuid REFERENCES accounts (id) ON DELETE SET NULL,
    added_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, source_id)
);
CREATE INDEX event_maps_source_idx ON event_maps (source_id);
