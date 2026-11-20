//! Reading what the host and Docker print. Pure functions, tested against output captured
//! from real systems (`tests/fixtures/`).

use serde_json::Value;

use crate::report::{ContainerReport, MemoryReport, OsInfo, ProcessReport};

// ------------------------------------------------------------------------------- /proc

/// `/proc/meminfo`. `None` when the kernel does not give `MemTotal` and `MemAvailable`.
pub fn meminfo(text: &str) -> Option<MemoryReport> {
    let kb = |name: &str| -> Option<u64> {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
    };
    Some(MemoryReport {
        total_bytes: kb("MemTotal")? * 1024,
        available_bytes: kb("MemAvailable")? * 1024,
        swap_total_bytes: kb("SwapTotal").unwrap_or(0) * 1024,
        swap_free_bytes: kb("SwapFree").unwrap_or(0) * 1024,
    })
}

/// The three load averages of `/proc/loadavg`.
pub fn loadavg(text: &str) -> Option<(f32, f32, f32)> {
    let mut parts = text.split_whitespace().map(|p| p.parse::<f32>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Seconds since boot, from `/proc/uptime`.
pub fn uptime(text: &str) -> Option<u64> {
    text.split_whitespace().next()?.parse::<f64>().ok().map(|s| s as u64)
}

/// CPU time counters of the whole machine, in clock ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub total: u64,
}

/// The aggregate `cpu` line of `/proc/stat`, and the number of processors listed.
pub fn cpu_stat(text: &str) -> Option<(CpuTimes, u32)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let values: Vec<u64> = line.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    // user nice system idle iowait irq softirq steal [guest guest_nice]: guest time is
    // already counted in user and nice.
    let total: u64 = values.iter().take(8).sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let cores = text.lines().filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit)).count() as u32;
    Some((CpuTimes { busy: total.saturating_sub(idle), total }, cores))
}

/// Share of time spent working between two readings, 0 to 100.
pub fn cpu_percent(before: CpuTimes, after: CpuTimes) -> Option<f32> {
    let total = after.total.checked_sub(before.total)?;
    let busy = after.busy.checked_sub(before.busy)?;
    (total > 0).then(|| (busy as f64 * 100.0 / total as f64).clamp(0.0, 100.0) as f32)
}

// ------------------------------------------------------------------------------- distribution

/// `/etc/os-release`.
pub fn os_release(text: &str) -> OsInfo {
    let value = |key: &str| -> String {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches(['"', '\'']).to_string())
            .unwrap_or_default()
    };
    OsInfo {
        id: value("ID").to_lowercase(),
        id_like: value("ID_LIKE").to_lowercase().split_whitespace().map(str::to_string).collect(),
        version_id: value("VERSION_ID"),
        pretty_name: value("PRETTY_NAME"),
    }
}

/// What `apt-get -s dist-upgrade` would install: how many packages, and how many of them
/// come from a security archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingUpdates {
    pub pending: u32,
    pub security: u32,
}

/// Reads the simulation printed by `apt-get -s dist-upgrade`. Each package it would install
/// or upgrade is one line:
///
/// ```text
/// Inst liblzma5 [5.4.1-0.2] (5.4.1-1+deb12u2 Debian-Security:12/oldstable-security [arm64])
/// Inst libc6 [2.35-0ubuntu3.1] (2.35-0ubuntu3.18 Ubuntu:22.04/jammy-updates, Ubuntu:22.04/jammy-security [arm64])
/// ```
///
/// The parenthesis names the archives the new version comes from. `None` when the text is
/// not such a simulation at all (apt failed), so a failure is never read as "up to date".
pub fn apt_simulation(text: &str) -> Option<PendingUpdates> {
    let mut pending = 0;
    let mut security = 0;
    for line in text.lines().filter(|l| l.starts_with("Inst ")) {
        pending += 1;
        let origin = line.split_once('(').and_then(|(_, rest)| rest.split_once(')')).map(|(inside, _)| inside).unwrap_or("");
        // Debian: "Debian-Security:12/stable-security"; Ubuntu: "Ubuntu:22.04/jammy-security",
        // and "UbuntuESM…/jammy-infra-security" for extended maintenance.
        if origin.split([' ', ',']).any(|word| word.to_lowercase().ends_with("-security") || word.starts_with("Debian-Security:")) {
            security += 1;
        }
    }
    // The summary line is always printed, even when nothing is pending.
    let summarised = text.lines().any(|l| l.contains(" upgraded, ") && l.contains(" newly installed, "));
    (summarised || pending > 0).then_some(PendingUpdates { pending, security })
}

// ------------------------------------------------------------------------------- Docker

fn text_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut node = v;
    for key in path {
        node = node.get(key)?;
    }
    node.as_str().filter(|s| !s.is_empty())
}

/// One element of `docker inspect <container>`. `None` when it carries no Compose service
/// label: it is not a container of a Compose project.
pub fn container(v: &Value) -> Option<ContainerReport> {
    Some(ContainerReport {
        service: text_at(v, &["Config", "Labels", "com.docker.compose.service"])?.to_string(),
        name: text_at(v, &["Name"]).unwrap_or_default().trim_start_matches('/').to_string(),
        state: text_at(v, &["State", "Status"]).unwrap_or("unknown").to_string(),
        health: text_at(v, &["State", "Health", "Status"]).map(str::to_string),
        restart_count: v.get("RestartCount").and_then(Value::as_u64).unwrap_or(0) as u32,
        exit_code: v.get("State").and_then(|s| s.get("ExitCode")).and_then(Value::as_i64),
        started_at: text_at(v, &["State", "StartedAt"]).filter(|s| !s.starts_with("0001-")).map(str::to_string),
        image: text_at(v, &["Config", "Image"]).unwrap_or_default().to_string(),
        image_id: text_at(v, &["Image"]).unwrap_or_default().to_string(),
        image_created: None,
        repo_digests: Vec::new(),
    })
}

/// The Compose project a container belongs to, from `docker inspect`.
pub fn compose_project(v: &Value) -> Option<&str> {
    text_at(v, &["Config", "Labels", "com.docker.compose.project"])
}

/// `(id, created, repo digests)` of one element of `docker image inspect`.
pub fn image(v: &Value) -> Option<(String, Option<String>, Vec<String>)> {
    let id = text_at(v, &["Id"])?.to_string();
    let digests = v
        .get("RepoDigests")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(|d| d.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Some((id, text_at(v, &["Created"]).map(str::to_string), digests))
}

/// The five Java processes TAK Server is made of, and how to recognise each in a process
/// list (the same patterns as the container's supervisor, deploy/takserver/entrypoint.sh).
pub const TAK_PROCESSES: &[(&str, &str)] = &[
    ("config", "spring.profiles.active=config"),
    ("messaging", "spring.profiles.active=messaging"),
    ("api", "spring.profiles.active=api"),
    ("retention", "takserver-retention.jar"),
    ("plugins", "takserver-pm.jar"),
];

/// Which of TAK Server's processes appear in the output of `docker top <container> -eo pid,args`.
pub fn tak_processes(top_output: &str) -> Vec<ProcessReport> {
    let commands: Vec<&str> = top_output.lines().skip(1).filter_map(|l| l.trim().split_once(char::is_whitespace)).map(|(_, c)| c.trim()).collect();
    TAK_PROCESSES
        .iter()
        .map(|(name, pattern)| ProcessReport {
            name: name.to_string(),
            running: commands.iter().any(|c| c.starts_with("java ") && c.contains(pattern)),
        })
        .collect()
}

// ------------------------------------------------------------------------------- files

/// Prefix of every archive written by deploy/scripts/backup.sh.
pub const BACKUP_PREFIX: &str = "teamtak-backup-";

/// True for a name the backup script gives its archives, and nothing else: the retention
/// rule and the listing never look at another file of the folder.
/// `teamtak-backup-20261119T031500Z.tar.gz`, with `.enc` appended when encrypted.
pub fn backup_file(name: &str) -> Option<bool> {
    let rest = name.strip_prefix(BACKUP_PREFIX)?;
    let (stamp, encrypted) = match rest.strip_suffix(".tar.gz.enc") {
        Some(stamp) => (stamp, true),
        None => (rest.strip_suffix(".tar.gz")?, false),
    };
    let bytes = stamp.as_bytes();
    let shape = bytes.len() == 16
        && bytes[8] == b'T'
        && bytes[15] == b'Z'
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[9..15].iter().all(u8::is_ascii_digit);
    shape.then_some(encrypted)
}

/// Version of a TAK Server ZIP from its file name:
/// `takserver-docker-hardened-5.8-RELEASE-103.zip` gives `5.8-RELEASE-103`.
pub fn zip_version(file_name: &str) -> Option<String> {
    let version = file_name.strip_prefix("takserver-docker-")?.strip_suffix(".zip")?;
    let version = version.strip_prefix("hardened-").unwrap_or(version);
    let plausible = version.starts_with(|c: char| c.is_ascii_digit())
        && version.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    plausible.then(|| version.to_string())
}

/// Value of `KEY` in a `.env` file read the way Docker Compose reads it.
pub fn env_value(env_text: &str, key: &str) -> Option<String> {
    env_text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.to_string())
        .next_back()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DEBIAN12: &str = include_str!("../tests/fixtures/apt-simulate-debian12.txt");
    const UBUNTU2204: &str = include_str!("../tests/fixtures/apt-simulate-ubuntu2204.txt");
    const UP_TO_DATE: &str = include_str!("../tests/fixtures/apt-simulate-uptodate.txt");

    #[test]
    fn memory_is_read_from_proc() {
        // Captured on the development machine's Docker VM.
        let text = "MemTotal:       16424164 kB\nMemFree:         9716616 kB\nMemAvailable:   10533508 kB\nBuffers:           1 kB\nSwapCached:            0 kB\nSwapTotal:       2097148 kB\nSwapFree:        1048574 kB\n";
        let m = meminfo(text).unwrap();
        assert_eq!(m.total_bytes, 16424164 * 1024);
        assert_eq!(m.available_bytes, 10533508 * 1024);
        assert_eq!((m.swap_total_bytes, m.swap_free_bytes), (2097148 * 1024, 1048574 * 1024));
        assert!((m.used_percent() - 35.87).abs() < 0.01);
        assert_eq!(meminfo("MemTotal: 5 kB\n"), None, "an old kernel without MemAvailable: unknown, not zero");
        assert_eq!(meminfo("MemTotal: 5 kB\nMemAvailable: 1 kB\n").unwrap().swap_total_bytes, 0);
    }

    #[test]
    fn load_uptime_and_cpu_are_read_from_proc() {
        assert_eq!(loadavg("1.10 0.68 0.70 4/1485 12\n"), Some((1.10, 0.68, 0.70)));
        assert_eq!(loadavg("garbage"), None);
        assert_eq!(uptime("210548.30 2899019.18\n"), Some(210548));
        let stat = "cpu  681530 0 376591 289901918 11845 0 92817 0 0 0\ncpu0 69045 0 43079 20594795 7242 0 56189 0 0 0\ncpu1 1 0 1 1 0 0 0 0 0 0\nintr 1 2 3\ncpufreq 9\n";
        let (times, cores) = cpu_stat(stat).unwrap();
        assert_eq!(cores, 2, "only the cpuN lines are processors");
        assert_eq!(times.total, 681530 + 376591 + 289901918 + 11845 + 92817);
        assert_eq!(times.busy, 681530 + 376591 + 92817);
        assert_eq!(cpu_stat("nothing"), None);
    }

    #[test]
    fn cpu_share_between_two_readings() {
        let before = CpuTimes { busy: 100, total: 1000 };
        assert_eq!(cpu_percent(before, CpuTimes { busy: 150, total: 1200 }), Some(25.0));
        assert_eq!(cpu_percent(before, before), None, "no time passed");
        assert_eq!(cpu_percent(before, CpuTimes { busy: 50, total: 900 }), None, "counters went back: the machine rebooted");
    }

    #[test]
    fn distributions_are_recognised_from_os_release() {
        let debian = os_release(include_str!("../tests/fixtures/os-release-debian12"));
        assert_eq!((debian.id.as_str(), debian.version_id.as_str()), ("debian", "12"));
        assert!(debian.pretty_name.starts_with("Debian GNU/Linux 12"));
        let ubuntu = os_release(include_str!("../tests/fixtures/os-release-ubuntu2204"));
        assert_eq!((ubuntu.id.as_str(), ubuntu.version_id.as_str()), ("ubuntu", "22.04"));
        assert_eq!(ubuntu.id_like, vec!["debian".to_string()]);
        // The Docker VM of the development machine.
        let orbstack = os_release("PRETTY_NAME=\"OrbStack\"\n");
        assert_eq!((orbstack.id.as_str(), orbstack.pretty_name.as_str()), ("", "OrbStack"));
        assert_eq!(os_release(""), OsInfo::default());
    }

    #[test]
    fn pending_updates_of_an_outdated_debian() {
        // debian:12.0 after `apt-get update`, simulated by the apt of Debian 13 as the agent does.
        assert_eq!(apt_simulation(DEBIAN12), Some(PendingUpdates { pending: 47, security: 9 }));
    }

    #[test]
    fn pending_updates_of_an_outdated_ubuntu() {
        // ubuntu:22.04 of January 2023: most updates are also published in jammy-security.
        assert_eq!(apt_simulation(UBUNTU2204), Some(PendingUpdates { pending: 61, security: 48 }));
    }

    #[test]
    fn an_up_to_date_system_has_nothing_pending() {
        assert_eq!(apt_simulation(UP_TO_DATE), Some(PendingUpdates { pending: 0, security: 0 }));
    }

    #[test]
    fn a_failed_simulation_is_unknown_not_up_to_date() {
        assert_eq!(apt_simulation(""), None);
        assert_eq!(apt_simulation("E: Could not open lock file /var/lib/dpkg/lock-frontend - open (13: Permission denied)\n"), None);
    }

    #[test]
    fn security_is_told_from_the_archive_not_from_the_package_name() {
        let text = "Inst libsecurity-tools [1.0] (1.1 Debian:12.15/oldstable [arm64])\n\
                    Inst openssl [3.0] (3.1 Debian-Security:12/stable-security [amd64])\n\
                    Inst esm-thing [1] (2 UbuntuESM:22.04/jammy-infra-security [amd64])\n\
                    Inst newdep (2.0 Debian:12.15/oldstable [all])\n\
                    Conf openssl (3.1 Debian-Security:12/stable-security [amd64])\n";
        assert_eq!(apt_simulation(text), Some(PendingUpdates { pending: 4, security: 2 }));
    }

    #[test]
    fn containers_are_read_from_docker_inspect() {
        let v = json!({
            "Id": "abc", "Name": "/teamtak-takserver-1", "RestartCount": 2, "Image": "sha256:img1",
            "State": {"Status": "running", "ExitCode": 0, "StartedAt": "2026-11-19T17:13:02.5Z", "Health": {"Status": "healthy"}},
            "Config": {"Image": "teamtak/takserver:5.8-RELEASE-103",
                       "Labels": {"com.docker.compose.project": "teamtak", "com.docker.compose.service": "takserver"}}
        });
        let c = container(&v).unwrap();
        assert_eq!((c.service.as_str(), c.name.as_str(), c.state.as_str()), ("takserver", "teamtak-takserver-1", "running"));
        assert_eq!((c.health.as_deref(), c.restart_count, c.exit_code), (Some("healthy"), 2, Some(0)));
        assert_eq!(c.image, "teamtak/takserver:5.8-RELEASE-103");
        assert_eq!(compose_project(&v), Some("teamtak"));

        let stopped = json!({"Name": "/x", "State": {"Status": "exited", "ExitCode": 137, "StartedAt": "0001-01-01T00:00:00Z"},
            "Config": {"Image": "caddy:2.11-alpine", "Labels": {"com.docker.compose.service": "caddy"}}});
        let c = container(&stopped).unwrap();
        assert_eq!((c.state.as_str(), c.health, c.exit_code, c.started_at), ("exited", None, Some(137), None));
        assert!(container(&json!({"Name": "/mapping-db-1", "Config": {"Labels": {}}})).is_none(), "not a Compose container");
    }

    #[test]
    fn images_are_read_from_docker_image_inspect() {
        let pulled = json!({"Id": "sha256:1", "Created": "2026-10-01T00:00:00Z", "RepoDigests": ["caddy@sha256:aa"]});
        assert_eq!(image(&pulled), Some(("sha256:1".into(), Some("2026-10-01T00:00:00Z".into()), vec!["caddy@sha256:aa".into()])));
        let built = json!({"Id": "sha256:2", "RepoDigests": []});
        assert_eq!(image(&built).unwrap().2, Vec::<String>::new());
    }

    #[test]
    fn tak_server_processes_are_found_in_docker_top() {
        // Captured from the running stack (TAK Server 5.8-RELEASE-103).
        let top = "PID                 COMMAND\n\
990414              /bin/bash /opt/teamtak/entrypoint.sh\n\
990492              {configureInDock} /bin/sh /opt/tak/configureInDocker.sh init\n\
990493              {tee} /usr/bin/coreutils --coreutils-prog-shebang=tee /usr/bin/tee -a /opt/tak/logs/takserver.log\n\
990508              java -jar -Xmx469m -Dspring.profiles.active=config takserver.war\n\
990509              java -jar -Xmx2607m -Dspring.profiles.active=messaging takserver.war\n\
990510              java -jar -Xmx2607m -Dspring.profiles.active=api -Dkeystore.pkcs12.legacy takserver.war\n\
990511              java -jar -Xmx547m takserver-retention.jar\n";
        let processes = tak_processes(top);
        let state: Vec<(&str, bool)> = processes.iter().map(|p| (p.name.as_str(), p.running)).collect();
        assert_eq!(state, vec![("config", true), ("messaging", true), ("api", true), ("retention", true), ("plugins", false)]);
        assert!(tak_processes("PID COMMAND\n").iter().all(|p| !p.running));
    }

    #[test]
    fn only_archives_of_the_backup_script_are_backups() {
        assert_eq!(backup_file("teamtak-backup-20261119T031500Z.tar.gz"), Some(false));
        assert_eq!(backup_file("teamtak-backup-20261119T031500Z.tar.gz.enc"), Some(true));
        for other in [
            "teamtak-backup-20261119T031500Z.tar.gz.partial",
            "teamtak-backup-latest.tar.gz",
            "teamtak-backup-2026-11-19.tar.gz",
            "notes.txt",
            "../teamtak-backup-20261119T031500Z.tar.gz",
            "teamtak-backup-20261119T031500Z.tar.gz/../x",
        ] {
            assert_eq!(backup_file(other), None, "{other}");
        }
    }

    #[test]
    fn version_of_a_tak_server_zip() {
        assert_eq!(zip_version("takserver-docker-hardened-5.8-RELEASE-103.zip").as_deref(), Some("5.8-RELEASE-103"));
        assert_eq!(zip_version("takserver-docker-5.9-RELEASE-12.zip").as_deref(), Some("5.9-RELEASE-12"));
        assert_eq!(zip_version("takserver-docker-hardened-.zip"), None);
        assert_eq!(zip_version("something-else.zip"), None);
        assert_eq!(zip_version("takserver-docker-hardened-5.8 RELEASE.zip"), None);
    }

    #[test]
    fn settings_are_read_from_the_env_file() {
        let env = "# comment\nMODE=local\nPUBLIC_HOST=localhost\nMAIL_FROM=TeamTAK <a=b@c>\nMODE=production\n";
        assert_eq!(env_value(env, "MODE").as_deref(), Some("production"), "the last one wins, as in Compose");
        assert_eq!(env_value(env, "MAIL_FROM").as_deref(), Some("TeamTAK <a=b@c>"));
        assert_eq!(env_value(env, "ABSENT"), None);
    }
}
