-- TeamTAK: what the teams sent to TAK Server through the platform.
--
-- TAK Server keeps the files (Enterprise Sync) and the missions; the platform reads them with
-- its administrator's certificate and shows each team what its devices would get. TAK Server
-- records every upload of the platform as sent by the platform's own account: this table says
-- who of which team sent it, so that a member may remove what they sent and a leader what the
-- team sent, and nothing of another team.

CREATE TABLE team_files (
    id          uuid PRIMARY KEY,
    team_id     uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    account_id  uuid REFERENCES accounts (id) ON DELETE SET NULL,
    hash        text NOT NULL,
    -- The upload in Enterprise Sync; NULL when TAK Server's listing did not show it at once.
    primary_key bigint,
    -- The mission it was added to; NULL for a data package of its own.
    mission     text,
    name        text NOT NULL,
    size        bigint NOT NULL,
    -- The TAK group it was stored for, or the mission's name.
    channel     text NOT NULL,
    uploaded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX team_files_team_idx ON team_files (team_id);
CREATE INDEX team_files_key_idx ON team_files (primary_key);
CREATE INDEX team_files_mission_idx ON team_files (mission, hash);
