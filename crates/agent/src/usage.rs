//! What each container of the stack uses: processor time and memory, read in the kernel's
//! control groups (the host's `/sys/fs/cgroup`, mounted read-only), and the space its data
//! takes on disk, its volumes and what it mounts from deploy/data.
//!
//! Docker gives each container a control group named after its identifier. Where it sits
//! depends on how Docker is set up: the usual places are tried, and a container found in
//! none of them is reported as unknown, never as idle.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::parse::{self, Mount};

/// Most files and folders counted under one mount of a service. Beyond it the size is
/// reported as unknown rather than walked for seconds at every report.
pub const DISK_WALK_LIMIT: usize = 200_000;

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Where a container's control group can be: with systemd managing the groups (Docker's
/// default on Debian and Ubuntu), then with Docker's own driver. Under cgroup v1 the same
/// path is found under each controller's hierarchy.
fn group_dirs(id: &str) -> [String; 2] {
    [format!("system.slice/docker-{id}.scope"), format!("docker/{id}")]
}

/// Processor time the container has used since it started, in microseconds.
pub fn cpu_usec(cgroup_root: &Path, id: &str) -> Option<u64> {
    for dir in group_dirs(id) {
        if let Some(stat) = read(&cgroup_root.join(&dir).join("cpu.stat")) {
            return parse::cgroup_cpu_usec(&stat);
        }
        for controller in ["cpu,cpuacct", "cpuacct"] {
            if let Some(ns) = read(&cgroup_root.join(controller).join(&dir).join("cpuacct.usage")) {
                return ns.trim().parse::<u64>().ok().map(|ns| ns / 1000);
            }
        }
    }
    None
}

/// Memory the container uses, as `docker stats` counts it.
pub fn memory_bytes(cgroup_root: &Path, id: &str) -> Option<u64> {
    for dir in group_dirs(id) {
        let v2 = cgroup_root.join(&dir);
        if let (Some(current), Some(stat)) = (read(&v2.join("memory.current")), read(&v2.join("memory.stat"))) {
            return parse::cgroup_memory(&current, &stat, "inactive_file");
        }
        let v1 = cgroup_root.join("memory").join(&dir);
        if let (Some(usage), Some(stat)) = (read(&v1.join("memory.usage_in_bytes")), read(&v1.join("memory.stat"))) {
            return parse::cgroup_memory(&usage, &stat, "total_inactive_file");
        }
    }
    None
}

/// The processor time of one container at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuReading {
    pub at: Instant,
    pub usec: u64,
}

/// Share of the whole machine the container used between two readings, 0 to 100. `None`
/// when the counter went back (the container was restarted in between).
pub fn cpu_share(before: CpuReading, after: CpuReading, cores: u32) -> Option<f32> {
    let used = after.usec.checked_sub(before.usec)?;
    let elapsed = after.at.checked_duration_since(before.at)?.as_micros() as f64;
    (elapsed > 0.0 && cores > 0).then(|| (used as f64 * 100.0 / (elapsed * cores as f64)).clamp(0.0, 100.0) as f32)
}

/// Readings of every running container, by the key given with its identifier.
pub fn read_cpu(cgroup_root: &Path, containers: &[(String, String)]) -> HashMap<String, CpuReading> {
    containers
        .iter()
        .filter_map(|(key, id)| Some((key.clone(), CpuReading { at: Instant::now(), usec: cpu_usec(cgroup_root, id)? })))
        .collect()
}

// ------------------------------------------------------------------------------- disk

/// The path under which the agent sees `host_path` through its own mounts: through the
/// mount with the longest source that holds it. `None` when the agent does not see it.
pub fn seen_as(host_path: &Path, own_mounts: &[Mount]) -> Option<PathBuf> {
    own_mounts
        .iter()
        .filter_map(|m| {
            let rest = host_path.strip_prefix(&m.source).ok()?;
            let seen = if rest.as_os_str().is_empty() { m.destination.clone() } else { m.destination.join(rest) };
            Some((m.source.components().count(), seen))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, seen)| seen)
}

/// What holds a service's data, as the agent sees it: its volumes, and what it mounts from
/// `data_dir` (deploy/data). Configuration files mounted from elsewhere in the repository
/// are not data. `None` when one of its volumes is out of the agent's sight (Docker keeping
/// its data elsewhere than in /var/lib): a size without it would be short.
pub fn data_paths(mounts: &[Mount], own_mounts: &[Mount], data_dir: &Path) -> Option<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for mount in mounts {
        match mount.kind.as_str() {
            "volume" => paths.push(seen_as(&mount.source, own_mounts)?),
            "bind" => paths.extend(seen_as(&mount.source, own_mounts).filter(|p| p.starts_with(data_dir))),
            _ => {}
        }
    }
    // A folder mounted inside another one is already counted with it.
    paths.sort();
    paths.dedup();
    let all = paths.clone();
    paths.retain(|p| !all.iter().any(|q| q != p && p.starts_with(q)));
    Some(paths)
}

/// Space a file, or a folder and everything under it, takes on disk, counted like `du`:
/// blocks allocated, a file with several links once. Symbolic links are not followed, and
/// what disappears during the walk (a database's temporary files) is skipped. `None` when
/// `path` cannot be read, or holds more than `limit` entries.
#[cfg(unix)]
pub fn disk_size(path: &Path, limit: usize) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let root = std::fs::symlink_metadata(path).ok()?;
    let mut linked: HashSet<(u64, u64)> = HashSet::new();
    let mut total = 0u64;
    let mut entries = 0usize;
    let mut pending = vec![(path.to_path_buf(), root)];
    while let Some((current, metadata)) = pending.pop() {
        entries += 1;
        if entries > limit {
            return None;
        }
        if metadata.nlink() > 1 && !metadata.is_dir() && !linked.insert((metadata.dev(), metadata.ino())) {
            continue;
        }
        total += metadata.blocks() * 512;
        if metadata.is_dir() {
            let Ok(children) = std::fs::read_dir(&current) else {
                if current == path {
                    return None;
                }
                continue;
            };
            for child in children.flatten() {
                if let Ok(metadata) = child.metadata() {
                    pending.push((child.path(), metadata));
                }
            }
        }
    }
    Some(total)
}

#[cfg(not(unix))]
pub fn disk_size(_path: &Path, _limit: usize) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn mount(kind: &str, source: &str, destination: &str) -> Mount {
        Mount { kind: kind.into(), source: source.into(), destination: destination.into() }
    }

    /// The agent's own mounts, as deploy/docker-compose.yml sets them.
    fn agent_mounts() -> Vec<Mount> {
        vec![
            mount("bind", "/srv/teamtak", "/repo"),
            mount("bind", "/etc", "/host/etc"),
            mount("bind", "/var/lib", "/host/var/lib"),
            mount("bind", "/run", "/host/run"),
            mount("bind", "/srv/teamtak/deploy/backups", "/repo/deploy/backups"),
            mount("bind", "/srv/teamtak/deploy/data/tak", "/repo/deploy/data/tak"),
            mount("bind", "/var/run/docker.sock", "/var/run/docker.sock"),
        ]
    }

    #[test]
    fn host_paths_are_found_through_the_agent_mounts() {
        let own = agent_mounts();
        let seen = |p: &str| seen_as(Path::new(p), &own);
        assert_eq!(seen("/var/lib/docker/volumes/teamtak_postgres-data/_data"), Some("/host/var/lib/docker/volumes/teamtak_postgres-data/_data".into()));
        assert_eq!(seen("/srv/teamtak/deploy/data/tak/logs"), Some("/repo/deploy/data/tak/logs".into()), "the closest mount");
        assert_eq!(seen("/srv/teamtak/deploy/data/app"), Some("/repo/deploy/data/app".into()));
        assert_eq!(seen("/srv/teamtak"), Some("/repo".into()));
        assert_eq!(seen("/srv/teamtak-other/data"), None, "a name that merely starts the same is another folder");
        assert_eq!(seen("/home/someone"), None);
    }

    #[test]
    fn a_service_data_is_its_volumes_and_its_folders_under_deploy_data() {
        let own = agent_mounts();
        let data = Path::new("/repo/deploy/data");
        let takserver = [
            mount("bind", "/srv/teamtak/deploy/data/tak/CoreConfig.xml", "/opt/tak/CoreConfig.xml"),
            mount("bind", "/srv/teamtak/deploy/data/tak/logs", "/opt/tak/logs"),
            mount("bind", "/srv/teamtak/deploy/data/tak/logs/archive", "/opt/tak/logs/archive"),
            mount("bind", "/srv/teamtak/deploy/takserver/entrypoint.sh", "/opt/teamtak/entrypoint.sh"),
        ];
        assert_eq!(
            data_paths(&takserver, &own, data),
            Some(vec![PathBuf::from("/repo/deploy/data/tak/CoreConfig.xml"), "/repo/deploy/data/tak/logs".into()]),
            "the script from the repository is not data; a folder inside another is counted once"
        );
        let postgres = [mount("volume", "/var/lib/docker/volumes/teamtak_postgres-data/_data", "/var/lib/postgresql/data")];
        assert_eq!(data_paths(&postgres, &own, data), Some(vec![PathBuf::from("/host/var/lib/docker/volumes/teamtak_postgres-data/_data")]));
        // Docker keeps its volumes elsewhere: the size would be short, so it is unknown.
        let moved = [mount("volume", "/data/docker/volumes/teamtak_postgres-data/_data", "/var/lib/postgresql/data")];
        assert_eq!(data_paths(&moved, &own, data), None);
        assert_eq!(data_paths(&[mount("tmpfs", "", "/tmp")], &own, data), Some(vec![]));
    }

    #[test]
    fn the_size_of_a_folder_is_counted_like_du() {
        let root = std::env::temp_dir().join(format!("tm-agent-du-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("base/16384")).unwrap();
        std::fs::write(root.join("base/16384/1259"), vec![1u8; 100_000]).unwrap();
        std::fs::write(root.join("PG_VERSION"), "17\n").unwrap();
        let alone = disk_size(&root, 1000).unwrap();
        assert!(alone >= 100_000, "{alone}");
        // A second link to the same file is not counted twice; a symbolic link is not followed.
        std::fs::hard_link(root.join("base/16384/1259"), root.join("again")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/usr", root.join("elsewhere")).unwrap();
        let after = disk_size(&root, 1000).unwrap();
        assert!(after < alone + 10_000, "{alone} then {after}");
        assert_eq!(disk_size(&root, 3), None, "too many entries: unknown, not a part of it");
        assert!(disk_size(&root.join("PG_VERSION"), 10).unwrap() > 0, "a file mounted alone");
        assert_eq!(disk_size(&root.join("gone"), 10), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn processor_share_between_two_readings() {
        let start = Instant::now();
        let at = |ms: u64, usec: u64| CpuReading { at: start + Duration::from_millis(ms), usec };
        // One full core of four for a minute: a quarter of the machine.
        assert_eq!(cpu_share(at(0, 0), at(60_000, 60_000_000), 4), Some(25.0));
        assert_eq!(cpu_share(at(0, 0), at(60_000, 0), 4), Some(0.0));
        assert_eq!(cpu_share(at(0, 9_000_000), at(60_000, 1_000), 4), None, "restarted in between");
        assert_eq!(cpu_share(at(0, 0), at(0, 10), 4), None);
        assert_eq!(cpu_share(at(0, 0), at(1_000, 1_000_000), 0), None);
    }

    #[test]
    fn control_groups_are_found_where_docker_puts_them() {
        let root = std::env::temp_dir().join(format!("tm-agent-cgroup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // systemd's place (cgroup v2), Docker's own (v2), and v1's controller hierarchies.
        let systemd = root.join("system.slice/docker-aaaa.scope");
        std::fs::create_dir_all(&systemd).unwrap();
        std::fs::write(systemd.join("cpu.stat"), "usage_usec 5000\nuser_usec 4000\n").unwrap();
        std::fs::write(systemd.join("memory.current"), "3000\n").unwrap();
        std::fs::write(systemd.join("memory.stat"), "anon 2000\ninactive_file 1000\n").unwrap();
        let docker = root.join("docker/bbbb");
        std::fs::create_dir_all(&docker).unwrap();
        std::fs::write(docker.join("cpu.stat"), "usage_usec 7\n").unwrap();
        for dir in ["cpu,cpuacct/docker/cccc", "memory/docker/cccc"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("cpu,cpuacct/docker/cccc/cpuacct.usage"), "9000000\n").unwrap();
        std::fs::write(root.join("memory/docker/cccc/memory.usage_in_bytes"), "500\n").unwrap();
        std::fs::write(root.join("memory/docker/cccc/memory.stat"), "inactive_file 1\ntotal_inactive_file 100\n").unwrap();

        assert_eq!((cpu_usec(&root, "aaaa"), memory_bytes(&root, "aaaa")), (Some(5000), Some(2000)));
        assert_eq!((cpu_usec(&root, "bbbb"), memory_bytes(&root, "bbbb")), (Some(7), None), "no memory figures: unknown");
        assert_eq!((cpu_usec(&root, "cccc"), memory_bytes(&root, "cccc")), (Some(9000), Some(400)));
        assert_eq!((cpu_usec(&root, "dddd"), memory_bytes(&root, "dddd")), (None, None));
        let readings = read_cpu(&root, &[("a".into(), "aaaa".into()), ("d".into(), "dddd".into())]);
        assert_eq!(readings.keys().collect::<Vec<_>>(), vec!["a"], "a container that cannot be read has no reading");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
