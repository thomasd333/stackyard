use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::{applog, paths};

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub tools: BTreeMap<String, ToolSettings>,
    #[serde(default)]
    pub ports: Ports,
    #[serde(default)]
    pub terminal: Terminal,
    #[serde(default)]
    pub ui: Ui,
    /// Virtual hosts, in the order they were added.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<Site>,
    /// Folder Stackyard last ran from; a different current folder means it was moved or copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Ports {
    pub apache: u16,
    pub mysql: u16,
}

impl Default for Ports {
    fn default() -> Self {
        Self { apache: 80, mysql: 3306 }
    }
}

/// "Open terminal" choice. `shell` is a plain string, not an enum, so an unknown value can't make the
/// whole file fail to parse (which would reset it to defaults); unknown shells fall back to cmd.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Terminal {
    /// One of `terminal::SHELLS`.
    pub shell: String,
    /// Open as a Windows Terminal tab when it is installed.
    pub windows_terminal: bool,
    /// Program for `shell == "custom"`, e.g. Cmder.exe.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub custom: String,
}

impl Default for Terminal {
    fn default() -> Self {
        Self { shell: "cmd".into(), windows_terminal: true, custom: String::new() }
    }
}

/// Appearance. Plain strings like `Terminal::shell`; unknown values mean "system".
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Ui {
    /// One of `i18n::THEMES`.
    pub theme: String,
    /// One of `i18n::LANGUAGES`, or "system".
    pub language: String,
}

impl Default for Ui {
    fn default() -> Self {
        Self { theme: "system".into(), language: "system".into() }
    }
}

/// A `<name>.localhost` virtual host (see `sites.rs`). Fields default so a hand-edited entry missing
/// one can't make the whole file fall back to defaults.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Site {
    /// Host name without the `.localhost` suffix.
    pub name: String,
    /// DocumentRoot, relative to the Stackyard folder when inside it.
    pub root: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct ToolSettings {
    /// Selected version directory name (see `tools::Install::label`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

fn file() -> PathBuf {
    paths::config_dir().join("settings.json")
}

/// Set when an unreadable settings.json was replaced; shown once in the window (see `take_recovery`).
static RECOVERY: Mutex<Option<String>> = Mutex::new(None);

/// The parse error of a settings.json that was set aside since the window last asked.
pub fn take_recovery() -> Option<String> {
    RECOVERY.lock().unwrap().take()
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Writes through a temp file and a rename, so a crash or power loss mid-write never leaves a half file.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let tmp = sibling(path, ".tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

fn parse(text: &str) -> serde_json::Result<Settings> {
    // A BOM (PowerShell 5.1, Notepad) is not an error.
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
}

impl Settings {
    /// Missing settings are defaults. An unreadable file (bad hand edit) is kept as `settings.json.broken`
    /// and replaced by the last good copy (`settings.json.bak`), or defaults, instead of being silently
    /// overwritten by the next save.
    pub fn load() -> Self {
        Self::load_from(&file())
    }

    fn load_from(path: &Path) -> Self {
        let from_backup = || std::fs::read_to_string(sibling(path, ".bak")).ok().and_then(|s| parse(&s).ok());
        match std::fs::read_to_string(path) {
            Ok(text) => match parse(&text) {
                Ok(settings) => settings,
                Err(e) => {
                    let restored = from_backup();
                    let what = if restored.is_some() { "restored the last good copy" } else { "using defaults" };
                    applog::error(format!("settings.json is invalid ({e}); saved as settings.json.broken, {what}"));
                    let settings = restored.unwrap_or_default();
                    // Rewrite it so this happens once, not on every load; the broken text is kept.
                    let kept = std::fs::write(sibling(path, ".broken"), &text);
                    if kept.is_ok()
                        && let Ok(json) = serde_json::to_string_pretty(&settings)
                    {
                        let _ = write_atomic(path, &json);
                    }
                    *RECOVERY.lock().unwrap() = Some(e.to_string());
                    settings
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => from_backup().unwrap_or_default(),
            Err(e) => {
                applog::error(format!("Cannot read settings.json: {e}"));
                from_backup().unwrap_or_default()
            }
        }
    }

    /// Writes defaults on first run so users have a file to edit (e.g. to change ports).
    pub fn ensure_file() {
        if !file().exists() {
            let _ = Self::load().save();
        }
    }

    pub fn save(&self) -> io::Result<()> {
        std::fs::create_dir_all(paths::config_dir())?;
        self.save_to(&file())
    }

    fn save_to(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        // The current file becomes the backup, but only while it is still valid.
        if std::fs::read_to_string(path).is_ok_and(|s| parse(&s).is_ok()) {
            std::fs::copy(path, sibling(path, ".bak"))?;
        }
        write_atomic(path, &json)
    }

    pub fn selected_version(&self, tool_id: &str) -> Option<&str> {
        self.tools.get(tool_id)?.version.as_deref()
    }

    pub fn set_version(&mut self, tool_id: &str, version: String) {
        self.tools.entry(tool_id.to_string()).or_default().version = Some(version);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("stackyard-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn save_keeps_previous_good_copy() {
        let path = temp_file("save");
        let mut s = Settings::default();
        s.ports.apache = 8081;
        s.save_to(&path).unwrap();
        s.ports.apache = 8082;
        s.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).ports.apache, 8082);
        assert_eq!(Settings::load_from(&sibling(&path, ".bak")).ports.apache, 8081);
        assert!(!sibling(&path, ".tmp").exists());
    }

    #[test]
    fn broken_file_is_kept_and_last_good_restored() {
        let path = temp_file("broken");
        let mut s = Settings::default();
        s.ports.mysql = 3307;
        s.save_to(&path).unwrap();
        s.save_to(&path).unwrap();
        std::fs::write(&path, r#"{"ports": {"mysql": 3308,}}"#).unwrap();

        assert_eq!(Settings::load_from(&path).ports.mysql, 3307);
        assert!(take_recovery().is_some());
        assert!(std::fs::read_to_string(sibling(&path, ".broken")).unwrap().contains("3308"));
        // The file was rewritten, so the next load is quiet.
        assert_eq!(Settings::load_from(&path).ports.mysql, 3307);
    }

    #[test]
    fn bom_is_accepted() {
        let path = temp_file("bom");
        std::fs::write(&path, "\u{feff}{\"ports\":{\"apache\":8090}}").unwrap();
        assert_eq!(Settings::load_from(&path).ports.apache, 8090);
    }
}
