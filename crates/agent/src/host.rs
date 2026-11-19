//! What the agent reads about the machine: `/proc`, the filesystems, the clock, and the
//! package manager's state.
//!
//! `/proc` of the agent's own container already describes the whole machine for what is
//! read here (memory, load, CPU time, uptime are not per-container). The rest comes from
//! three host directories mounted read-only under [`HostPaths::root`]: `/etc`, `/var/lib`
//! and `/run`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;

use crate::parse::{self, CpuTimes};
use crate::report::{percent, ClockReport, CpuReport, DiskReport, HostReport, OsInfo, UpdatesReport};

#[derive(Debug, Clone)]
pub struct HostPaths {
    /// Where the host's `/etc`, `/var/lib` and `/run` are mounted.
    pub root: PathBuf,
    pub proc: PathBuf,
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

// ------------------------------------------------------------------------------- package manager

/// The distribution-specific part: pending updates and the reboot flag. Only the Debian
/// family is known; elsewhere the report says "unknown" rather than guess.
#[async_trait]
pub trait Distribution: Send + Sync {
    fn package_manager(&self) -> &'static str;
    async fn updates(&self, host_root: &Path) -> UpdatesReport;
}

/// The distribution of `os`, when the agent knows how to read its package state.
pub fn distribution_for(os: &OsInfo) -> Option<Box<dyn Distribution>> {
    let debian_family = matches!(os.id.as_str(), "debian" | "ubuntu") || os.id_like.iter().any(|like| like == "debian");
    debian_family.then(|| Box::new(Apt) as Box<dyn Distribution>)
}

/// Debian and Ubuntu.
///
/// The agent never runs anything on the host. It runs its own `apt-get` in simulation mode
/// (`-s`), pointed at the host's package database and package lists, both read-only: the
/// answer is what `apt-get dist-upgrade` would install on the host. The lists are the ones
/// the host last downloaded (its own daily `apt-get update`); their age is reported.
pub struct Apt;

impl Apt {
    fn arguments(host_root: &Path, cache_dir: &Path) -> Vec<String> {
        let host = |path: &str| host_root.join(path).display().to_string();
        vec![
            "-s".into(),
            // Read-only state: no lock can be taken, and none is needed for a simulation.
            "-o".into(),
            "Debug::NoLocking=1".into(),
            "-o".into(),
            format!("Dir::State={}", host("var/lib/apt")),
            "-o".into(),
            format!("Dir::State::status={}", host("var/lib/dpkg/status")),
            // The host's sources and pinning, but not its apt.conf.d: hooks written for the
            // host make no sense here.
            "-o".into(),
            format!("Dir::Etc::sourcelist={}", host("etc/apt/sources.list")),
            "-o".into(),
            format!("Dir::Etc::sourceparts={}", host("etc/apt/sources.list.d")),
            "-o".into(),
            format!("Dir::Etc::preferences={}", host("etc/apt/preferences")),
            "-o".into(),
            format!("Dir::Etc::preferencesparts={}", host("etc/apt/preferences.d")),
            "-o".into(),
            format!("Dir::Cache={}", cache_dir.display()),
            "dist-upgrade".into(),
        ]
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[async_trait]
impl Distribution for Apt {
    fn package_manager(&self) -> &'static str {
        "apt"
    }

    async fn updates(&self, host_root: &Path) -> UpdatesReport {
        let mut report = UpdatesReport { manager: Some("apt".into()), ..Default::default() };

        // The flag Debian and Ubuntu packages raise when they need a reboot to take effect.
        let run = host_root.join("run");
        if run.is_dir() {
            report.reboot_required = Some(run.join("reboot-required").exists());
            report.reboot_packages = read(&run.join("reboot-required.pkgs"))
                .map(|text| {
                    let mut packages: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect();
                    packages.sort();
                    packages.dedup();
                    packages
                })
                .unwrap_or_default();
        }

        let lists = host_root.join("var/lib/apt/lists");
        let stamps = [host_root.join("var/lib/apt/periodic/update-success-stamp"), lists.clone(), lists.join("partial")];
        report.lists_age_seconds = stamps
            .iter()
            .filter_map(|p| modified(p))
            .max()
            .and_then(|newest| SystemTime::now().duration_since(newest).ok())
            .map(|age| age.as_secs());

        if !host_root.join("var/lib/dpkg/status").is_file() {
            report.error = Some("the host's package database is not readable".into());
            return report;
        }
        let cache_dir = std::env::temp_dir().join("tak-manager-agent-apt");
        let _ = std::fs::create_dir_all(&cache_dir);
        let run = tokio::process::Command::new("apt-get")
            .args(Self::arguments(host_root, &cache_dir))
            .env("LC_ALL", "C")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output();
        match tokio::time::timeout(Duration::from_secs(90), run).await {
            Err(_) => report.error = Some("apt-get did not answer in time".into()),
            Ok(Err(e)) => report.error = Some(format!("apt-get could not be run: {e}")),
            Ok(Ok(output)) => match parse::apt_simulation(&String::from_utf8_lossy(&output.stdout)) {
                Some(found) => {
                    report.pending = Some(found.pending);
                    report.security = Some(found.security);
                }
                None => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output");
                    report.error = Some(format!("apt-get: {}", first.chars().take(200).collect::<String>()));
                }
            },
        }
        report
    }
}

// ------------------------------------------------------------------------------- clock

/// `STA_UNSYNC` of `<sys/timex.h>`: the kernel has not been told the clock is synchronised.
const STA_UNSYNC: i32 = 0x0040;
/// Return value of `adjtimex` when the clock is not synchronised.
const TIME_ERROR: i32 = 5;

/// Reads what `adjtimex(2)` returned. This is the kernel's own view, the one `timedatectl`
/// prints as "System clock synchronized": an NTP client (chrony, ntpd, systemd-timesyncd)
/// clears the flag once it disciplines the clock.
pub fn clock_from_adjtimex(result: i32, status: i32, max_error_us: i64) -> ClockReport {
    if result < 0 {
        return ClockReport::default();
    }
    ClockReport {
        synchronized: Some(result != TIME_ERROR && status & STA_UNSYNC == 0),
        max_error_ms: u64::try_from(max_error_us).ok().map(|us| us / 1000),
    }
}

#[cfg(target_os = "linux")]
pub fn clock() -> ClockReport {
    // SAFETY: `timex` is plain data; with `modes` at zero the call only reads.
    let mut timex: libc::timex = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::adjtimex(&mut timex) };
    clock_from_adjtimex(result, timex.status, timex.maxerror as i64)
}

#[cfg(not(target_os = "linux"))]
pub fn clock() -> ClockReport {
    ClockReport::default()
}

// ------------------------------------------------------------------------------- disks

/// Space of a filesystem the way `df` counts it: the used share is of what an ordinary
/// program can use, the blocks reserved for root left out.
pub fn disk_usage(blocks: u64, free: u64, available: u64, block_size: u64) -> (u64, u64, f32) {
    let used = blocks.saturating_sub(free);
    (blocks * block_size, available * block_size, percent(used, used + available))
}

#[cfg(unix)]
fn filesystem(path: &Path) -> Option<(u64, (u64, u64, f32))> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let device = std::fs::metadata(path).ok()?.dev();
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` fills the structure it is given; the path is a valid C string.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    #[allow(clippy::unnecessary_cast)]
    let usage = disk_usage(stat.f_blocks as u64, stat.f_bfree as u64, stat.f_bavail as u64, stat.f_frsize as u64);
    Some((device, usage))
}

/// One entry per filesystem among the places given, in order. Places on the same filesystem
/// are merged under the first name.
pub fn disks(places: &[(&str, PathBuf)]) -> Vec<DiskReport> {
    let mut found: Vec<(u64, DiskReport)> = Vec::new();
    for (name, path) in places {
        let Some((device, (total, available, used_percent))) = filesystem(path) else { continue };
        if total == 0 {
            continue;
        }
        match found.iter_mut().find(|(d, _)| *d == device) {
            Some((_, disk)) => disk.also.push(name.to_string()),
            None => found.push((
                device,
                DiskReport { name: name.to_string(), also: Vec::new(), total_bytes: total, available_bytes: available, used_percent },
            )),
        }
    }
    found.into_iter().map(|(_, disk)| disk).collect()
}

// ------------------------------------------------------------------------------- the whole host

/// Reads the CPU counters twice a moment apart when there is no earlier reading.
async fn cpu(paths: &HostPaths, previous: Option<CpuTimes>) -> (CpuReport, Option<CpuTimes>) {
    let stat = |p: &HostPaths| read(&p.proc.join("stat")).and_then(|t| parse::cpu_stat(&t));
    let Some((mut now, cores)) = stat(paths) else { return (CpuReport::default(), None) };
    let before = match previous {
        Some(before) => before,
        None => {
            let first = now;
            tokio::time::sleep(Duration::from_millis(250)).await;
            now = stat(paths).map(|(t, _)| t).unwrap_or(now);
            first
        }
    };
    let load = read(&paths.proc.join("loadavg")).and_then(|t| parse::loadavg(&t));
    let report = CpuReport {
        cores: Some(cores).filter(|c| *c > 0),
        percent: parse::cpu_percent(before, now),
        load1: load.map(|l| l.0),
        load5: load.map(|l| l.1),
        load15: load.map(|l| l.2),
    };
    (report, Some(now))
}

/// Everything about the machine except the package state, which is slower and cached by
/// the caller. Returns the CPU counters to hand back at the next call.
pub async fn collect(paths: &HostPaths, places: &[(&str, PathBuf)], previous_cpu: Option<CpuTimes>) -> (HostReport, Option<CpuTimes>) {
    let os = read(&paths.root.join("etc/os-release")).map(|t| parse::os_release(&t)).unwrap_or_default();
    let (cpu, cpu_times) = cpu(paths, previous_cpu).await;
    let report = HostReport {
        os,
        kernel: read(&paths.proc.join("sys/kernel/osrelease")).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        uptime_seconds: read(&paths.proc.join("uptime")).and_then(|t| parse::uptime(&t)),
        cpu,
        memory: read(&paths.proc.join("meminfo")).and_then(|t| parse::meminfo(&t)),
        disks: disks(places),
        clock: clock(),
        updates: UpdatesReport::default(),
    };
    (report, cpu_times)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(id: &str, like: &[&str]) -> OsInfo {
        OsInfo { id: id.into(), id_like: like.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn only_the_debian_family_has_a_known_package_manager() {
        assert_eq!(distribution_for(&os("debian", &[])).unwrap().package_manager(), "apt");
        assert_eq!(distribution_for(&os("ubuntu", &["debian"])).unwrap().package_manager(), "apt");
        assert_eq!(distribution_for(&os("raspbian", &["debian"])).unwrap().package_manager(), "apt");
        for other in [os("fedora", &[]), os("rocky", &["rhel", "centos", "fedora"]), os("alpine", &[]), os("", &[])] {
            assert!(distribution_for(&other).is_none(), "{}: unknown, never guessed", other.id);
        }
    }

    #[test]
    fn apt_reads_the_host_state_and_writes_nowhere_on_it() {
        let args = Apt::arguments(Path::new("/host"), Path::new("/tmp/cache"));
        assert_eq!(args[0], "-s", "simulation only");
        assert_eq!(args.last().unwrap(), "dist-upgrade");
        assert!(args.contains(&"Dir::State::status=/host/var/lib/dpkg/status".to_string()));
        assert!(args.contains(&"Dir::State=/host/var/lib/apt".to_string()));
        assert!(args.contains(&"Dir::Cache=/tmp/cache".to_string()), "the only thing written is the agent's own cache");
        assert!(!args.iter().any(|a| a == "upgrade" || a == "install" || a == "update" || a == "-y"));
    }

    #[tokio::test]
    async fn the_reboot_flag_is_read_from_the_host_run_directory() {
        let root = std::env::temp_dir().join(format!("tm-agent-host-{}", std::process::id()));
        std::fs::create_dir_all(root.join("run")).unwrap();
        // No package database: the counts stay unknown, the flag is still read.
        let before = Apt.updates(&root).await;
        assert_eq!((before.reboot_required, before.pending, before.security), (Some(false), None, None));
        assert!(before.error.is_some());
        std::fs::write(root.join("run/reboot-required"), "*** System restart required ***\n").unwrap();
        std::fs::write(root.join("run/reboot-required.pkgs"), "linux-image-6.1.0-28-amd64\nlibc6\nlibc6\n").unwrap();
        let after = Apt.updates(&root).await;
        assert_eq!(after.reboot_required, Some(true));
        assert_eq!(after.reboot_packages, vec!["libc6".to_string(), "linux-image-6.1.0-28-amd64".to_string()]);
        std::fs::remove_dir_all(&root).unwrap();
        // Without the host's /run nothing is claimed.
        assert_eq!(Apt.updates(&root).await.reboot_required, None);
    }

    #[test]
    fn the_kernel_says_whether_the_clock_is_synchronised() {
        // TIME_OK with the flag cleared: an NTP client is in charge.
        assert_eq!(clock_from_adjtimex(0, 0x2001, 500_000), ClockReport { synchronized: Some(true), max_error_ms: Some(500) });
        // TIME_ERROR, STA_UNSYNC set: nothing synchronises this clock (max error at its ceiling).
        assert_eq!(clock_from_adjtimex(5, 0x0040, 16_000_000), ClockReport { synchronized: Some(false), max_error_ms: Some(16_000) });
        assert_eq!(clock_from_adjtimex(0, 0x0041, 1).synchronized, Some(false), "the flag alone is enough");
        // The call itself refused (a sandbox that forbids it): unknown.
        assert_eq!(clock_from_adjtimex(-1, 0, 0), ClockReport::default());
    }

    #[test]
    fn disk_usage_is_counted_like_df() {
        // 100 blocks of 4 KiB, 30 free of which 25 for ordinary programs: 70 used of 95 usable.
        let (total, available, used) = disk_usage(100, 30, 25, 4096);
        assert_eq!((total, available), (409_600, 102_400));
        assert!((used - 73.68).abs() < 0.01, "{used}");
        assert_eq!(disk_usage(0, 0, 0, 4096).2, 0.0);
    }

    #[test]
    fn places_on_one_filesystem_are_reported_once() {
        let here = std::env::temp_dir();
        let found = disks(&[("root", here.clone()), ("data", here.clone()), ("gone", PathBuf::from("/nonexistent/tak-manager"))]);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].name.as_str(), found[0].also.clone()), ("root", vec!["data".to_string()]));
        assert!(found[0].total_bytes > 0 && found[0].used_percent <= 100.0);
    }
}
