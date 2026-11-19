//! Housekeeping of TAK Server's logs (deploy/data/tak/logs).
//!
//! TAK Server rolls most of its logs by itself every day into `<name>.<date>.log.gz` and
//! keeps 90 of them, with no limit on their size. Two things are left to do: delete rolled
//! files sooner, and cut the live files that are never rolled (the supervisor's own
//! `takserver.log`, appended to at every start) when they grow.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::SystemTime;

use crate::report::RotateResult;

/// A live log is cut once it is larger than this.
pub const LIVE_LIMIT_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_KEEP_DAYS: u32 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFile {
    pub name: String,
    pub size: u64,
    pub age_days: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Rolled files past their time.
    pub delete: Vec<String>,
    /// Live files to compress aside and empty.
    pub rotate: Vec<String>,
}

/// Decides what to do with the files of the log folder. Only files that look like logs are
/// ever touched: `*.log` (live) and `*.log.gz` (rolled).
pub fn plan(files: &[LogFile], keep_days: u32, live_limit: u64) -> Plan {
    let mut plan = Plan::default();
    for file in files {
        if file.name.ends_with(".log.gz") {
            if file.age_days > keep_days as u64 {
                plan.delete.push(file.name.clone());
            }
        } else if file.name.ends_with(".log") && file.size > live_limit {
            plan.rotate.push(file.name.clone());
        }
    }
    plan
}

fn list(dir: &Path) -> std::io::Result<Vec<LogFile>> {
    let now = SystemTime::now();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        let age = metadata.modified().ok().and_then(|m| now.duration_since(m).ok()).map(|d| d.as_secs() / 86_400).unwrap_or(0);
        files.push(LogFile { name: entry.file_name().to_string_lossy().into_owned(), size: metadata.len(), age_days: age });
    }
    Ok(files)
}

/// Total size of the folder's files; `None` when it cannot be read.
pub fn folder_size(dir: &Path) -> Option<u64> {
    list(dir).ok().map(|files| files.iter().map(|f| f.size).sum())
}

/// Compresses a live log next to itself, then empties it. The file is written by a running
/// process in append mode, so it is emptied in place rather than renamed; lines written
/// between the copy and the truncation are lost.
fn rotate_live(dir: &Path, name: &str, stamp: &str) -> std::io::Result<()> {
    let source = dir.join(name);
    let stem = name.strip_suffix(".log").unwrap_or(name);
    let target = dir.join(format!("{stem}.{stamp}.log.gz"));
    let mut input = File::open(&source)?;
    let mut encoder = flate2::write::GzEncoder::new(File::create(&target)?, flate2::Compression::default());
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        encoder.write_all(&buffer[..n])?;
    }
    encoder.finish()?.sync_all()?;
    std::fs::OpenOptions::new().write(true).open(&source)?.set_len(0)
}

/// Applies [`plan`] to the folder.
pub fn rotate(dir: &Path, keep_days: u32, live_limit: u64, stamp: &str) -> std::io::Result<RotateResult> {
    let files = list(dir)?;
    let plan = plan(&files, keep_days, live_limit);
    let mut result = RotateResult::default();
    for name in &plan.delete {
        let size = files.iter().find(|f| &f.name == name).map(|f| f.size).unwrap_or(0);
        std::fs::remove_file(dir.join(name))?;
        result.deleted_files += 1;
        result.freed_bytes += size;
    }
    for name in &plan.rotate {
        rotate_live(dir, name, stamp)?;
        result.rotated_files += 1;
    }
    result.remaining_bytes = folder_size(dir).unwrap_or(0);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, size: u64, age_days: u64) -> LogFile {
        LogFile { name: name.into(), size, age_days }
    }

    #[test]
    fn old_rolled_logs_go_and_large_live_logs_are_cut() {
        let files = [
            file("takserver-messaging.log", 10, 0),
            file("takserver-messaging.2026-10-01.log.gz", 5_000, 49),
            file("takserver-messaging.2026-11-18.log.gz", 5_000, 1),
            file("takserver-api.2026-10-20.log.gz", 5_000, 30),
            file("takserver.log", 200, 0),
        ];
        let plan = plan(&files, 30, 100);
        assert_eq!(plan.delete, vec!["takserver-messaging.2026-10-01.log.gz".to_string()], "30 days old is kept, 49 is not");
        assert_eq!(plan.rotate, vec!["takserver.log".to_string()]);
    }

    #[test]
    fn nothing_but_logs_is_ever_touched() {
        let files = [
            file("CoreConfig.xml", u64::MAX, 400),
            file("takserver.jks", u64::MAX, 400),
            file("notes.gz", 10, 400),
            file("dump.log.gz.keep", 10, 400),
        ];
        assert_eq!(plan(&files, 0, 0), Plan::default());
    }

    #[test]
    fn a_live_log_is_compressed_aside_and_emptied_in_place() {
        let dir = std::env::temp_dir().join(format!("tm-agent-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("takserver.log"), "line one\nline two\n".repeat(100)).unwrap();
        std::fs::write(dir.join("small.log"), "short").unwrap();
        std::fs::write(dir.join("keep.txt"), "x".repeat(5000)).unwrap();

        let result = rotate(&dir, 30, 1000, "20261119T030000Z").unwrap();
        assert_eq!((result.rotated_files, result.deleted_files), (1, 0));
        assert_eq!(std::fs::metadata(dir.join("takserver.log")).unwrap().len(), 0, "emptied, not removed: the writer keeps its file");
        assert_eq!(std::fs::read_to_string(dir.join("small.log")).unwrap(), "short");
        assert_eq!(std::fs::metadata(dir.join("keep.txt")).unwrap().len(), 5000);

        let mut text = String::new();
        flate2::read::GzDecoder::new(File::open(dir.join("takserver.20261119T030000Z.log.gz")).unwrap()).read_to_string(&mut text).unwrap();
        assert_eq!(text, "line one\nline two\n".repeat(100));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
