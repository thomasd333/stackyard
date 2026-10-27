//! Reading and editing the *user* PATH (HKCU\Environment), which needs no admin rights.

use std::borrow::Cow;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Foundation::{LPARAM, WPARAM};
use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use winreg::RegKey;
use winreg::RegValue;
use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, RegType};

use crate::paths;

const ENV_KEY: &str = "Environment";
const SYSTEM_ENV_KEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const MAX_BACKUPS: usize = 20;

/// The user PATH split into entries, plus the registry type it was stored with.
pub struct UserPath {
    pub entries: Vec<String>,
    vtype: RegType,
}

impl UserPath {
    pub fn read() -> io::Result<Self> {
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(ENV_KEY, KEY_READ)?;
        match key.get_raw_value("Path") {
            Ok(value) => Ok(Self {
                entries: split(&decode_utf16(&value.bytes)),
                vtype: value.vtype,
            }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Ok(Self { entries: Vec::new(), vtype: REG_EXPAND_SZ })
            }
            Err(e) => Err(e),
        }
    }

    /// Writes the PATH back (keeping its registry type), after saving a backup of the old value,
    /// and notifies running programs so new terminals pick it up.
    pub fn write(&self) -> io::Result<()> {
        backup_current()?;
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(ENV_KEY, KEY_READ | KEY_WRITE)?;
        let value = RegValue { bytes: Cow::Owned(encode_utf16(&self.entries.join(";"))), vtype: self.vtype.clone() };
        key.set_raw_value("Path", &value)?;
        broadcast_change();
        Ok(())
    }

    pub fn contains_under(&self, dir: &Path) -> bool {
        self.entries.iter().any(|e| is_under(e, dir))
    }

    pub fn remove_under(&mut self, dir: &Path) {
        self.entries.retain(|e| !is_under(e, dir));
    }

    /// Puts `dirs` at the front, in order, so our tools win over anything else in the user PATH.
    pub fn prepend(&mut self, dirs: &[std::path::PathBuf]) {
        let new: Vec<String> = dirs.iter().map(|d| d.to_string_lossy().into_owned()).collect();
        self.entries.retain(|e| !new.iter().any(|n| normalize(n) == normalize(e)));
        self.entries.splice(0..0, new);
    }
}

/// System + user PATH as a new process started from Explorer would see it now. Our own environment is
/// stale when Stackyard has been running in the tray since before a PATH change.
pub fn current_full_path() -> String {
    let read = |hkey, subkey| -> Option<Vec<String>> {
        let value = RegKey::predef(hkey).open_subkey_with_flags(subkey, KEY_READ).ok()?.get_raw_value("Path").ok()?;
        Some(split(&decode_utf16(&value.bytes)).iter().map(|e| expand(e)).collect())
    };
    match (read(HKEY_LOCAL_MACHINE, SYSTEM_ENV_KEY), read(HKEY_CURRENT_USER, ENV_KEY)) {
        (None, None) => std::env::var("PATH").unwrap_or_default(),
        (system, user) => [system.unwrap_or_default(), user.unwrap_or_default()].concat().join(";"),
    }
}

/// First `dir\exe` that exists among the entries of a PATH string.
pub fn split_find(path: &str, exe: &str) -> Option<std::path::PathBuf> {
    split(path).iter().map(|d| Path::new(d.trim_matches('"')).join(exe)).find(|p| p.is_file())
}

/// True when `entry` is `dir` or a subdirectory of it (case-insensitive, slash-agnostic).
pub fn is_under(entry: &str, dir: &Path) -> bool {
    let entry = normalize(&expand(entry));
    let dir = normalize(&dir.to_string_lossy());
    entry == dir || entry.starts_with(&format!("{dir}\\"))
}

pub fn exists(entry: &str) -> bool {
    Path::new(&expand(entry)).is_dir()
}

fn normalize(p: &str) -> String {
    p.trim().trim_matches('"').replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

fn split(raw: &str) -> Vec<String> {
    raw.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Expands `%VARS%` (REG_EXPAND_SZ entries such as `%USERPROFILE%\bin`).
fn expand(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let src: Vec<u16> = s.encode_utf16().chain(Some(0)).collect();
    let mut buf = vec![0u16; 1024];
    loop {
        let n = unsafe { ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 {
            return s.to_string();
        }
        if n <= buf.len() {
            return String::from_utf16_lossy(&buf[..n - 1]);
        }
        buf.resize(n, 0);
    }
}

fn decode_utf16(bytes: &[u8]) -> String {
    let wide: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
    let end = wide.iter().position(|&c| c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

fn encode_utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(Some(0)).flat_map(u16::to_le_bytes).collect()
}

/// Saves the current raw PATH to `config/backups/path-<unix-time>.txt`, keeping the newest few.
fn backup_current() -> io::Result<()> {
    let dir = paths::config_dir().join("backups");
    std::fs::create_dir_all(&dir)?;
    let current = UserPath::read()?.entries.join(";");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    std::fs::write(dir.join(format!("path-{stamp}.txt")), current)?;

    let mut backups: Vec<_> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("path-")))
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(MAX_BACKUPS);
    for old in &backups[..excess] {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

fn broadcast_change() {
    let param: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
    let mut result = 0usize;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0 as WPARAM,
            param.as_ptr() as LPARAM,
            SMTO_ABORTIFHUNG,
            5000,
            &mut result,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_matching() {
        let dir = Path::new(r"D:\SERVER\bin\php");
        assert!(is_under(r"D:\SERVER\bin\php\php-8.3", dir));
        assert!(is_under(r"d:/server/bin/php/", dir));
        assert!(!is_under(r"D:\SERVER\bin\phpmyadmin", dir));
    }

    #[test]
    fn utf16_roundtrip() {
        let s = r"C:\Zażółć;D:\x";
        assert_eq!(decode_utf16(&encode_utf16(s)), s);
    }
}
