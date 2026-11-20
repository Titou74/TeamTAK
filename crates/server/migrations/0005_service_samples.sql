-- The history of each service of the stack, next to the machine's.
--
-- From this version on, metric_samples gets one row a minute instead of one every five
-- minutes: about 20,000 rows over the 14 days kept. Each of those samples has here one row
-- per service the host agent measured, about 160,000 rows for the eight services of the
-- stack. Both are purged once a day with the same retention
-- (crates/server/src/monitor/maintenance.rs).
CREATE TABLE service_samples (
    -- The same instant as the machine's sample it goes with.
    sampled_at   timestamptz NOT NULL,
    -- Name of the Compose service.
    service      text NOT NULL,
    -- Share of the whole machine's processor since the previous look, 0 to 100.
    cpu_percent  real,
    -- Memory in use, the file cache the kernel can take back left out.
    memory_bytes bigint,
    -- Space its volumes and its folders under deploy/data take.
    disk_bytes   bigint,
    PRIMARY KEY (sampled_at, service)
);
