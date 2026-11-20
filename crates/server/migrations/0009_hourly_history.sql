-- The long history: one row an hour for the machine and for each service, kept ten years, so
-- the charts reach back past the 14 days of minute samples (0003, 0005). Each hour keeps the
-- average of its minutes and the highest of them, so a peak stays visible however far back
-- one looks.
--
-- The platform updates the current hour after each minute sample and purges both tables once a
-- day (crates/server/src/monitor/maintenance.rs): about 88,000 rows for the machine over ten
-- years, and as many per service.
CREATE TABLE metric_hours (
    -- The start of the hour, UTC.
    hour               timestamptz PRIMARY KEY,
    -- The latest minute sample of the hour.
    sampled_at         timestamptz NOT NULL,
    cpu_percent        real,
    cpu_percent_max    real,
    load1              real,
    memory_percent     real,
    memory_percent_max real,
    swap_percent       real,
    swap_percent_max   real,
    disk_percent       real,
    disk_percent_max   real,
    devices            real,
    devices_max        integer
);

CREATE TABLE service_hours (
    hour             timestamptz NOT NULL,
    service          text NOT NULL,
    sampled_at       timestamptz NOT NULL,
    cpu_percent      real,
    cpu_percent_max  real,
    memory_bytes     bigint,
    memory_bytes_max bigint,
    disk_bytes       bigint,
    disk_bytes_max   bigint,
    PRIMARY KEY (hour, service)
);

-- The minute samples already kept give the first 14 days.
INSERT INTO metric_hours
SELECT date_bin('1 hour', sampled_at, TIMESTAMPTZ '2000-01-01 00:00:00+00'), max(sampled_at),
       avg(cpu_percent), max(cpu_percent), avg(load1), avg(memory_percent), max(memory_percent),
       avg(swap_percent), max(swap_percent), avg(disk_percent), max(disk_percent), avg(devices), max(devices)
  FROM metric_samples
 GROUP BY 1;

INSERT INTO service_hours
SELECT date_bin('1 hour', sampled_at, TIMESTAMPTZ '2000-01-01 00:00:00+00'), service, max(sampled_at),
       avg(cpu_percent), max(cpu_percent), round(avg(memory_bytes)), max(memory_bytes),
       round(avg(disk_bytes)), max(disk_bytes)
  FROM service_samples
 GROUP BY 1, 2;
