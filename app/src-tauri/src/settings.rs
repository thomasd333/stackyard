use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub tools: BTreeMap<String, ToolSettings>,
    #[serde(default)]
    pub ports: Ports,
    #[serde(default)]
    pub terminal: Terminal,
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

#[derive(Default, Serialize, Deserialize)]
pub struct ToolSettings {
    /// Selected version directory name (see `tools::Install::label`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

fn file() -> PathBuf {
    paths::config_dir().join("settings.json")
}

impl Settings {
    /// Missing or unreadable settings fall back to defaults rather than failing startup.
    pub fn load() -> Self {
        std::fs::read_to_string(file())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Writes defaults on first run so users have a file to edit (e.g. to change ports).
    pub fn ensure_file() {
        if !file().exists() {
            let _ = Self::load().save();
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(paths::config_dir())?;
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(file(), json)
    }

    pub fn selected_version(&self, tool_id: &str) -> Option<&str> {
        self.tools.get(tool_id)?.version.as_deref()
    }

    pub fn set_version(&mut self, tool_id: &str, version: String) {
        self.tools.entry(tool_id.to_string()).or_default().version = Some(version);
    }
}
