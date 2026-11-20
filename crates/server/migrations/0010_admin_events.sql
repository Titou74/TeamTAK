-- Events organised by the administrators.
--
-- An administrator who runs a gathering of teams creates the event and puts the teams in it
-- directly. Such an event has no host team: its host columns stay empty, and every team in
-- it is a row of event_teams with the status 'accepted' from the moment it is added.
--
-- An event of a team whose host was deleted also has no host_team_id, but keeps the name of
-- its host: `organizer` is what tells the two apart.
ALTER TABLE events ADD COLUMN organizer text NOT NULL DEFAULT 'team' CHECK (organizer IN ('team', 'admin'));
ALTER TABLE events ALTER COLUMN host_team_name DROP NOT NULL;
ALTER TABLE events ADD CONSTRAINT events_organizer_host_check CHECK (
    (organizer = 'team' AND host_team_name IS NOT NULL)
    OR (organizer = 'admin' AND host_team_id IS NULL AND host_team_name IS NULL)
);
