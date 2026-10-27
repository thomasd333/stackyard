//! Keeps things working after the Stackyard folder is moved: re-points user PATH entries from the
//! old location and refreshes the CLI php.ini, which both hold absolute paths.

use std::path::Path;

use crate::applog;
use crate::confgen;
use crate::env_path::UserPath;
use crate::paths;
use crate::services;
use crate::settings::Settings;

/// Runs once at app start, before any service is started.
pub fn on_startup() {
    if !paths::root_is_supported() {
        applog::warn(paths::UNSUPPORTED_ROOT_MESSAGE);
    }
    let current = paths::app_root();
    let current_str = current.to_string_lossy().into_owned();
    let mut settings = Settings::load();

    if let Some(previous) = settings.root.clone().filter(|p| !same_dir(p, &current_str)) {
        applog::info(format!("Stackyard folder changed from {previous} to {current_str}"));
        migrate_path(&Path::new(&previous).join("bin"), &paths::bin_dir());
    }
    if settings.root.as_deref() != Some(current_str.as_str()) {
        settings.root = Some(current_str);
        if let Err(e) = settings.save() {
            applog::warn(format!("Cannot save settings: {e}"));
        }
    }

    // php.exe from PATH reads the php.ini in its own folder; keep its paths current even if
    // Apache is never started in this location.
    if let Some(php) = services::selected("php", &settings)
        && let Err(e) = confgen::ensure_layout().and_then(|_| confgen::write_php(&php.dir))
    {
        applog::warn(format!("Cannot refresh php.ini for {}: {e}", php.label));
    }
}

fn migrate_path(old_bin: &Path, new_bin: &Path) {
    let mut user_path = match UserPath::read() {
        Ok(p) => p,
        Err(e) => return applog::warn(format!("PATH: cannot read user PATH: {e}")),
    };
    let (entries, moved) = relocate_entries(&user_path.entries, old_bin, new_bin, |dir| Path::new(dir).is_dir());
    if moved == 0 {
        return;
    }
    user_path.entries = entries;
    match user_path.write() {
        Ok(()) => applog::info(format!("PATH: re-pointed {moved} entries to {}", new_bin.display())),
        Err(e) => applog::error(format!("PATH: re-pointing entries failed: {e}")),
    }
}

/// Re-points entries under `old_bin` to the same place under `new_bin`. Entries whose directory still
/// exists are kept: the old folder was copied rather than moved and may still be in use. Returns the
/// new list and how many entries changed.
fn relocate_entries(
    entries: &[String],
    old_bin: &Path,
    new_bin: &Path,
    exists: impl Fn(&str) -> bool,
) -> (Vec<String>, usize) {
    let old = old_bin.to_string_lossy().replace('/', "\\");
    let old = old.trim_end_matches('\\');
    let new = new_bin.to_string_lossy();
    let new = new.trim_end_matches('\\');

    let mut result: Vec<String> = Vec::with_capacity(entries.len());
    let mut moved = 0;
    for entry in entries {
        let clean = entry.trim().trim_matches('"').replace('/', "\\");
        // `get` instead of slicing: a byte offset may fall inside a non-ASCII character.
        let rest = clean
            .get(..old.len())
            .filter(|head| head.eq_ignore_ascii_case(old))
            .and_then(|_| clean.get(old.len()..))
            .filter(|rest| rest.is_empty() || rest.starts_with('\\'));
        match rest {
            Some(rest) if !exists(entry) => {
                let relocated = format!("{new}{rest}");
                if !result.iter().chain(entries.iter()).any(|e| e.eq_ignore_ascii_case(&relocated)) {
                    result.push(relocated);
                }
                moved += 1;
            }
            _ => result.push(entry.clone()),
        }
    }
    (result, moved)
}

fn same_dir(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn moved_entries_are_repointed() {
        let entries = s(&[r"C:\Windows", r"D:\SERVER\bin\php\php-8.3", r"d:/server/bin/git/cmd", r"D:\SERVERX\bin\x"]);
        let (out, moved) = relocate_entries(&entries, Path::new(r"D:\SERVER\bin"), Path::new(r"E:\Stack yard\bin"), |_| false);
        assert_eq!(moved, 2);
        assert_eq!(out, s(&[r"C:\Windows", r"E:\Stack yard\bin\php\php-8.3", r"E:\Stack yard\bin\git\cmd", r"D:\SERVERX\bin\x"]));
    }

    #[test]
    fn copied_folder_keeps_existing_entries() {
        let entries = s(&[r"D:\SERVER\bin\php\php-8.3"]);
        let (out, moved) = relocate_entries(&entries, Path::new(r"D:\SERVER\bin"), Path::new(r"E:\S\bin"), |_| true);
        assert_eq!((out, moved), (entries.clone(), 0));
    }

    #[test]
    fn no_duplicates_when_new_entry_already_present() {
        let entries = s(&[r"E:\S\bin\php\8.3", r"D:\SERVER\bin\php\8.3"]);
        let (out, moved) = relocate_entries(&entries, Path::new(r"D:\SERVER\bin"), Path::new(r"E:\S\bin"), |_| false);
        assert_eq!((out, moved), (s(&[r"E:\S\bin\php\8.3"]), 1));
    }

    #[test]
    fn non_ascii_paths_do_not_panic() {
        let entries = s(&[r"D:\Zażółć\x", r"D:\SERVER\bin\ź"]);
        let (out, moved) = relocate_entries(&entries, Path::new(r"D:\SERVER\bin"), Path::new(r"D:\Mój\bin"), |_| false);
        assert_eq!((out, moved), (s(&[r"D:\Zażółć\x", r"D:\Mój\bin\ź"]), 1));
    }
}
