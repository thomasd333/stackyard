//! Stackyard's own log (`logs\stackyard.log`) and size-based rotation of log files.
//!
//! Rotation happens only while nothing holds a file open: service logs right before the service
//! starts, our own log when the app starts.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use windows_sys::Win32::System::SystemInformation::GetLocalTime;

use crate::paths;

/// Logs above this size are moved to `logs\archive\` on the next start.
const MAX_LOG_BYTES: u64 = 10 * 1024 * 1024;
/// Archived copies kept per log file.
const KEEP_ARCHIVES: usize = 5;

const APP_LOG: &str = "stackyard.log";

static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn info(msg: impl AsRef<str>) {
    write("INFO", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    write("WARN", msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    write("ERROR", msg.as_ref());
}

/// Rotates our own log, records the start and logs panics (with location) before they abort.
pub fn init() {
    rotate(&[APP_LOG]);
    info(format!("Stackyard {} started, root {}", env!("CARGO_PKG_VERSION"), paths::app_root().display()));
    std::panic::set_hook(Box::new(|panic| error(format!("Panic: {panic}"))));
}

fn write(level: &str, msg: &str) {
    // Unit tests must not write into the real logs\ folder.
    if cfg!(test) {
        return;
    }
    // A poisoned lock only means another thread panicked mid-write; logging must go on.
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = paths::logs_dir();
    let _ = fs::create_dir_all(&dir);
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(dir.join(APP_LOG)) else { return };
    let stamp = timestamp();
    for (i, line) in msg.trim_end().lines().enumerate() {
        let indent = if i == 0 { "" } else { "    " };
        let _ = writeln!(file, "{stamp} {level:<5} {indent}{line}");
    }
}

/// Moves each log in `logs\` that exceeds the size limit to `logs\archive\<name>-<time>.log`
/// and prunes old archives.
pub fn rotate(names: &[&str]) {
    rotate_in(&paths::logs_dir(), names, MAX_LOG_BYTES);
}

fn rotate_in(dir: &Path, names: &[&str], max_bytes: u64) {
    let archive = dir.join("archive");
    for name in names {
        let file = dir.join(name);
        let Ok(meta) = fs::metadata(&file) else { continue };
        if meta.len() <= max_bytes {
            continue;
        }
        let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let target = archive.join(format!("{stem}-{}.log", file_stamp()));
        let moved = fs::create_dir_all(&archive).and_then(|_| fs::rename(&file, &target));
        match moved {
            // Our own log may be the one being rotated, so log after the move.
            Ok(()) => info(format!("Rotated {name} ({} MB) to {}", meta.len() / 1_048_576, target.display())),
            Err(e) => warn(format!("Cannot rotate {name}: {e}")),
        }
        prune(&archive, &format!("{stem}-"));
    }
}

/// Keeps the newest `KEEP_ARCHIVES` archives starting with `prefix` (names sort by time).
fn prune(archive: &Path, prefix: &str) {
    let mut old: Vec<_> = fs::read_dir(archive)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(prefix)))
        .collect();
    old.sort();
    let excess = old.len().saturating_sub(KEEP_ARCHIVES);
    for file in &old[..excess] {
        let _ = fs::remove_file(file);
    }
}

fn local_time() -> windows_sys::Win32::Foundation::SYSTEMTIME {
    let mut time = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut time) };
    time
}

/// "2026-10-27 07:12:03"
fn timestamp() -> String {
    let t = local_time();
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// "20261027-071203", safe for file names and sortable.
fn file_stamp() -> String {
    let t = local_time();
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_large_logs_and_keeps_small_ones() {
        let dir = std::env::temp_dir().join(format!("stackyard-rotate-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("archive")).unwrap();
        fs::write(dir.join("big.log"), "x".repeat(100)).unwrap();
        fs::write(dir.join("small.log"), "x").unwrap();
        // Pre-existing archives: the oldest ones must be pruned down to KEEP_ARCHIVES.
        for i in 0..KEEP_ARCHIVES + 2 {
            fs::write(dir.join(format!("archive/big-2000010{i}-000000.log")), "old").unwrap();
        }

        rotate_in(&dir, &["big.log", "small.log", "missing.log"], 10);

        assert!(!dir.join("big.log").exists());
        assert!(dir.join("small.log").exists());
        let archived: Vec<String> = fs::read_dir(dir.join("archive"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(archived.len(), KEEP_ARCHIVES);
        assert!(archived.iter().any(|n| !n.starts_with("big-200001")), "new archive kept: {archived:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
