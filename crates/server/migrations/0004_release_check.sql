-- What GitHub last said about TeamTAK's latest release, and when it was asked: it is asked
-- at most twice a day, and the answer survives a restart.
ALTER TABLE monitor_state
    ADD COLUMN release_check      jsonb NOT NULL DEFAULT '{}',
    ADD COLUMN release_checked_at timestamptz;
