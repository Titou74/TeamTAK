//! The history behind the charts: one sample a minute kept 14 days (`metric_samples`,
//! `service_samples`), and one row an hour kept ten years (`metric_hours`, `service_hours`)
//! with the average and the highest minute of each hour. A chart over a period that reaches
//! further back than the minutes is drawn from the hours.

use serde::Serialize;
use sqlx::{PgPool, Result};
use time::{Duration, OffsetDateTime};

use super::maintenance::{HOUR_SAMPLES_DAYS, METRIC_SAMPLES_DAYS};

/// Most points a chart is given.
pub const MAX_POINTS: i64 = 300;

/// Where the points of a chart come from, and how many seconds each one stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub hourly: bool,
    pub bucket_seconds: i64,
}

/// The points of a chart from `from` to `to`: from the minute samples while they still cover
/// `from`, in whole minutes; else from the hours, in whole hours. Never more than `MAX_POINTS`.
pub fn plan(from: OffsetDateTime, to: OffsetDateTime, now: OffsetDateTime) -> Plan {
    let span = (to - from).whole_seconds().max(60);
    let hourly = from < now - Duration::days(METRIC_SAMPLES_DAYS as i64);
    let unit = if hourly { 3600 } else { 60 };
    let units = (span + unit - 1) / unit;
    Plan { hourly, bucket_seconds: ((units + MAX_POINTS - 1) / MAX_POINTS) * unit }
}

/// The furthest back a chart can start.
pub fn oldest(now: OffsetDateTime) -> OffsetDateTime {
    now - Duration::days(HOUR_SAMPLES_DAYS as i64)
}

/// One point of the machine's chart: the average of the samples it stands for, and the
/// highest of them, at the time of the latest.
#[derive(Serialize, sqlx::FromRow)]
pub struct Sample {
    #[serde(with = "time::serde::rfc3339")]
    pub sampled_at: OffsetDateTime,
    pub cpu_percent: Option<f32>,
    pub cpu_percent_max: Option<f32>,
    pub load1: Option<f32>,
    pub memory_percent: Option<f32>,
    pub memory_percent_max: Option<f32>,
    pub swap_percent: Option<f32>,
    pub swap_percent_max: Option<f32>,
    pub disk_percent: Option<f32>,
    pub disk_percent_max: Option<f32>,
    pub devices: Option<i32>,
    pub devices_max: Option<i32>,
}

/// One point of a service's chart, the same way.
#[derive(Serialize, sqlx::FromRow)]
pub struct ServiceSample {
    #[serde(skip)]
    pub service: String,
    #[serde(with = "time::serde::rfc3339")]
    pub sampled_at: OffsetDateTime,
    pub cpu_percent: Option<f32>,
    pub cpu_percent_max: Option<f32>,
    pub memory_bytes: Option<i64>,
    pub memory_bytes_max: Option<i64>,
    pub disk_bytes: Option<i64>,
    pub disk_bytes_max: Option<i64>,
}

#[derive(Serialize)]
pub struct ServiceHistory {
    pub service: String,
    pub samples: Vec<ServiceSample>,
}

// Points are grouped from 2000-01-01 UTC, so the same period always gives the same points. A
// minute's highest is itself; an hour keeps its own (`…_max`).

const MACHINE_MINUTES: &str = "
    SELECT max(sampled_at) AS sampled_at,
           avg(cpu_percent)::real AS cpu_percent, max(cpu_percent) AS cpu_percent_max, avg(load1)::real AS load1,
           avg(memory_percent)::real AS memory_percent, max(memory_percent) AS memory_percent_max,
           avg(swap_percent)::real AS swap_percent, max(swap_percent) AS swap_percent_max,
           avg(disk_percent)::real AS disk_percent, max(disk_percent) AS disk_percent_max,
           round(avg(devices))::int4 AS devices, max(devices) AS devices_max
      FROM metric_samples WHERE sampled_at > $1 AND sampled_at <= $2
     GROUP BY date_bin(make_interval(secs => $3), sampled_at, TIMESTAMPTZ '2000-01-01 00:00:00+00')
     ORDER BY sampled_at";

const MACHINE_HOURS: &str = "
    SELECT max(sampled_at) AS sampled_at,
           avg(cpu_percent)::real AS cpu_percent, max(cpu_percent_max) AS cpu_percent_max, avg(load1)::real AS load1,
           avg(memory_percent)::real AS memory_percent, max(memory_percent_max) AS memory_percent_max,
           avg(swap_percent)::real AS swap_percent, max(swap_percent_max) AS swap_percent_max,
           avg(disk_percent)::real AS disk_percent, max(disk_percent_max) AS disk_percent_max,
           round(avg(devices))::int4 AS devices, max(devices_max) AS devices_max
      FROM metric_hours WHERE sampled_at > $1 AND sampled_at <= $2
     GROUP BY date_bin(make_interval(secs => $3), hour, TIMESTAMPTZ '2000-01-01 00:00:00+00')
     ORDER BY sampled_at";

const SERVICES_MINUTES: &str = "
    SELECT service, max(sampled_at) AS sampled_at,
           avg(cpu_percent)::real AS cpu_percent, max(cpu_percent) AS cpu_percent_max,
           round(avg(memory_bytes))::int8 AS memory_bytes, max(memory_bytes) AS memory_bytes_max,
           round(avg(disk_bytes))::int8 AS disk_bytes, max(disk_bytes) AS disk_bytes_max
      FROM service_samples WHERE sampled_at > $1 AND sampled_at <= $2
     GROUP BY service, date_bin(make_interval(secs => $3), sampled_at, TIMESTAMPTZ '2000-01-01 00:00:00+00')
     ORDER BY service, sampled_at";

const SERVICES_HOURS: &str = "
    SELECT service, max(sampled_at) AS sampled_at,
           avg(cpu_percent)::real AS cpu_percent, max(cpu_percent_max) AS cpu_percent_max,
           round(avg(memory_bytes))::int8 AS memory_bytes, max(memory_bytes_max) AS memory_bytes_max,
           round(avg(disk_bytes))::int8 AS disk_bytes, max(disk_bytes_max) AS disk_bytes_max
      FROM service_hours WHERE sampled_at > $1 AND sampled_at <= $2
     GROUP BY service, date_bin(make_interval(secs => $3), hour, TIMESTAMPTZ '2000-01-01 00:00:00+00')
     ORDER BY service, sampled_at";

/// The machine's points, oldest first.
pub async fn machine(db: &PgPool, from: OffsetDateTime, to: OffsetDateTime, plan: Plan) -> Result<Vec<Sample>> {
    sqlx::query_as(if plan.hourly { MACHINE_HOURS } else { MACHINE_MINUTES })
        .bind(from)
        .bind(to)
        .bind(plan.bucket_seconds as f64)
        .fetch_all(db)
        .await
}

/// Each service's points, oldest first, the services by name.
pub async fn services(db: &PgPool, from: OffsetDateTime, to: OffsetDateTime, plan: Plan) -> Result<Vec<ServiceHistory>> {
    let rows: Vec<ServiceSample> = sqlx::query_as(if plan.hourly { SERVICES_HOURS } else { SERVICES_MINUTES })
        .bind(from)
        .bind(to)
        .bind(plan.bucket_seconds as f64)
        .fetch_all(db)
        .await?;
    let mut services: Vec<ServiceHistory> = Vec::new();
    for row in rows {
        match services.last_mut() {
            Some(last) if last.service == row.service => last.samples.push(row),
            _ => services.push(ServiceHistory { service: row.service.clone(), samples: vec![row] }),
        }
    }
    Ok(services)
}

/// Brings the hour of `at` up to date from its minute samples, for the machine and each
/// service. Called after each sample: the current hour is never more than a minute behind.
pub async fn roll_up(db: &PgPool, at: OffsetDateTime) -> Result<()> {
    sqlx::query(
        "INSERT INTO metric_hours
         SELECT date_bin('1 hour', $1::timestamptz, TIMESTAMPTZ '2000-01-01 00:00:00+00'), max(sampled_at),
                avg(cpu_percent), max(cpu_percent), avg(load1), avg(memory_percent), max(memory_percent),
                avg(swap_percent), max(swap_percent), avg(disk_percent), max(disk_percent), avg(devices), max(devices)
           FROM metric_samples
          WHERE sampled_at >= date_bin('1 hour', $1::timestamptz, TIMESTAMPTZ '2000-01-01 00:00:00+00')
            AND sampled_at < date_bin('1 hour', $1::timestamptz, TIMESTAMPTZ '2000-01-01 00:00:00+00') + interval '1 hour'
         HAVING count(*) > 0
         ON CONFLICT (hour) DO UPDATE SET
                sampled_at = EXCLUDED.sampled_at, cpu_percent = EXCLUDED.cpu_percent, cpu_percent_max = EXCLUDED.cpu_percent_max,
                load1 = EXCLUDED.load1, memory_percent = EXCLUDED.memory_percent, memory_percent_max = EXCLUDED.memory_percent_max,
                swap_percent = EXCLUDED.swap_percent, swap_percent_max = EXCLUDED.swap_percent_max,
                disk_percent = EXCLUDED.disk_percent, disk_percent_max = EXCLUDED.disk_percent_max,
                devices = EXCLUDED.devices, devices_max = EXCLUDED.devices_max",
    )
    .bind(at)
    .execute(db)
    .await?;
    sqlx::query(
        "INSERT INTO service_hours
         SELECT date_bin('1 hour', $1::timestamptz, TIMESTAMPTZ '2000-01-01 00:00:00+00'), service, max(sampled_at),
                avg(cpu_percent), max(cpu_percent), round(avg(memory_bytes)), max(memory_bytes), round(avg(disk_bytes)), max(disk_bytes)
           FROM service_samples
          WHERE sampled_at >= date_bin('1 hour', $1::timestamptz, TIMESTAMPTZ '2000-01-01 00:00:00+00')
            AND sampled_at < date_bin('1 hour', $1::timestamptz, TIMESTAMPTZ '2000-01-01 00:00:00+00') + interval '1 hour'
          GROUP BY service
         ON CONFLICT (hour, service) DO UPDATE SET
                sampled_at = EXCLUDED.sampled_at, cpu_percent = EXCLUDED.cpu_percent, cpu_percent_max = EXCLUDED.cpu_percent_max,
                memory_bytes = EXCLUDED.memory_bytes, memory_bytes_max = EXCLUDED.memory_bytes_max,
                disk_bytes = EXCLUDED.disk_bytes, disk_bytes_max = EXCLUDED.disk_bytes_max",
    )
    .bind(at)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn recent_periods_come_from_the_minutes_older_ones_from_the_hours() {
        let now = datetime!(2026-11-20 12:00 UTC);
        let last = |hours: i64| plan(now - Duration::hours(hours), now, now);
        assert_eq!(last(1), Plan { hourly: false, bucket_seconds: 60 }, "the last hour: every sample");
        assert_eq!(last(24), Plan { hourly: false, bucket_seconds: 300 });
        assert_eq!(last(24 * 7), Plan { hourly: false, bucket_seconds: 34 * 60 });
        assert_eq!(last(24 * 30), Plan { hourly: true, bucket_seconds: 3 * 3600 });
        assert_eq!(last(24 * 180), Plan { hourly: true, bucket_seconds: 15 * 3600 });
        // A short period long ago: one point an hour, the finest the hours give.
        let old = now - Duration::days(40);
        assert_eq!(plan(old, old + Duration::hours(6), now), Plan { hourly: true, bucket_seconds: 3600 });
        // A short period of last week: every minute.
        let recent = now - Duration::days(6);
        assert_eq!(plan(recent, recent + Duration::minutes(90), now), Plan { hourly: false, bucket_seconds: 60 });
    }

    #[test]
    fn a_chart_never_has_more_than_the_most_points() {
        let now = datetime!(2026-11-20 12:00 UTC);
        for minutes in [1, 59, 61, 299, 301, 1440, 20_159, 20_161, 525_600, HOUR_SAMPLES_DAYS as i64 * 1440] {
            let from = now - Duration::minutes(minutes);
            let p = plan(from, now, now);
            let points = (now - from).whole_seconds() / p.bucket_seconds;
            assert!(points <= MAX_POINTS, "{minutes} minutes: {points} points");
            assert_eq!(p.bucket_seconds % if p.hourly { 3600 } else { 60 }, 0);
        }
    }
}
