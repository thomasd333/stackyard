use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::applog;
use crate::confgen;
use crate::env_path::{self, UserPath};
use crate::paths;
use crate::services::{self, ServiceInfo, Services};
use crate::settings::{self, Settings};
use crate::terminal;
use crate::tools::{self, Install, ToolDef};

/// Version strings are cached per executable dir: spawning `php -v` etc. on every refresh is wasteful.
#[derive(Default)]
pub struct VersionCache(Mutex<HashMap<PathBuf, Option<String>>>);

type CmdResult<T> = Result<T, String>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    id: &'static str,
    name: &'static str,
    versions: Vec<String>,
    selected: Option<String>,
    version_text: Option<String>,
    in_path: bool,
    path_dirs: Vec<String>,
}

#[derive(Serialize)]
pub struct PathEntry {
    value: String,
    ours: bool,
    exists: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    root: String,
    /// False when the root path has characters PHP/MySQL can't handle (see `paths::root_is_supported`).
    root_supported: bool,
    tools: Vec<ToolInfo>,
    user_path: Vec<PathEntry>,
    terminal: TerminalInfo,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalInfo {
    shell: String,
    windows_terminal: bool,
    custom: String,
    wt_available: bool,
    bash_available: bool,
}

/// The selected install, falling back to the newest one when the saved choice is gone.
pub(crate) fn selected_install(tool: &ToolDef, installs: Vec<Install>, settings: &Settings) -> Option<Install> {
    let wanted = settings.selected_version(tool.id);
    let mut installs = installs.into_iter();
    let first = installs.next()?;
    if wanted.is_none_or(|w| w == first.label) {
        return Some(first);
    }
    installs.find(|i| Some(i.label.as_str()) == wanted).or(Some(first))
}

/// Install dir of the selected Git, if any.
fn git_dir(settings: &Settings) -> Option<PathBuf> {
    let git = tools::find("git")?;
    selected_install(git, git.installs(&paths::bin_dir()), settings).map(|i| i.dir)
}

fn build_state(cache: &VersionCache) -> CmdResult<AppState> {
    let bin = paths::bin_dir();
    let settings = Settings::load();
    let user_path = UserPath::read().map_err(|e| format!("Cannot read user PATH: {e}"))?;

    let selected: Vec<(&ToolDef, Vec<String>, Option<Install>)> = tools::TOOLS
        .iter()
        .map(|t| {
            let installs = t.installs(&bin);
            let labels = installs.iter().map(|i| i.label.clone()).collect();
            (t, labels, selected_install(t, installs, &settings))
        })
        .collect();

    // Fill missing versions in parallel; each probe is a process spawn.
    let missing: Vec<(&ToolDef, PathBuf)> = {
        let known = cache.0.lock().unwrap();
        selected
            .iter()
            .filter_map(|(t, _, i)| i.as_ref().map(|i| (*t, i.dir.clone())))
            .filter(|(_, dir)| !known.contains_key(dir))
            .collect()
    };
    let probed: Vec<(PathBuf, Option<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = missing
            .iter()
            .map(|(t, dir)| s.spawn(move || (dir.clone(), t.read_version(dir))))
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    let mut known = cache.0.lock().unwrap();
    known.extend(probed);

    let tools = selected
        .into_iter()
        .map(|(t, versions, install)| ToolInfo {
            id: t.id,
            name: t.name,
            versions,
            in_path: user_path.contains_under(&t.tool_dir(&bin)),
            path_dirs: install
                .as_ref()
                .map(|i| t.path_dirs_for(&i.dir).iter().map(|d| d.to_string_lossy().into_owned()).collect())
                .unwrap_or_default(),
            version_text: install.as_ref().and_then(|i| known.get(&i.dir).cloned().flatten()),
            selected: install.map(|i| i.label),
        })
        .collect();

    let terminal = TerminalInfo {
        wt_available: terminal::windows_terminal().is_some(),
        bash_available: terminal::git_bash(git_dir(&settings).as_deref()).is_some(),
        shell: settings.terminal.shell.clone(),
        windows_terminal: settings.terminal.windows_terminal,
        custom: settings.terminal.custom.clone(),
    };

    Ok(AppState {
        root: paths::app_root().to_string_lossy().into_owned(),
        root_supported: paths::root_is_supported(),
        tools,
        user_path: user_path
            .entries
            .iter()
            .map(|e| PathEntry { value: e.clone(), ours: env_path::is_under(e, &bin), exists: env_path::exists(e) })
            .collect(),
        terminal,
    })
}

/// Removes every PATH entry of `tool` (any version) and, if `enable`, adds the selected version's dirs.
fn apply_path(user_path: &mut UserPath, tool: &ToolDef, enable: bool, settings: &Settings) -> CmdResult<()> {
    let bin = paths::bin_dir();
    user_path.remove_under(&tool.tool_dir(&bin));
    if enable {
        let install = selected_install(tool, tool.installs(&bin), settings)
            .ok_or_else(|| format!("{} is not installed", tool.name))?;
        if tool.id == "php" {
            confgen::ensure_layout()?;
            confgen::write_php(&install.dir)?;
        }
        user_path.prepend(&tool.path_dirs_for(&install.dir));
    }
    Ok(())
}

fn tool_by_id(id: &str) -> CmdResult<&'static ToolDef> {
    tools::find(id).ok_or_else(|| format!("Unknown tool: {id}"))
}

/// Writes the user PATH and records `what` changed in the app log.
fn write_path(user_path: &UserPath, what: &str) -> CmdResult<()> {
    match user_path.write() {
        Ok(()) => {
            applog::info(format!("PATH: {what}"));
            Ok(())
        }
        Err(e) => {
            applog::error(format!("PATH: {what} failed: {e}"));
            Err(format!("Cannot write user PATH: {e}"))
        }
    }
}

#[tauri::command]
pub fn get_state(cache: State<VersionCache>) -> CmdResult<AppState> {
    build_state(&cache)
}

#[tauri::command]
pub fn refresh(cache: State<VersionCache>) -> CmdResult<AppState> {
    cache.0.lock().unwrap().clear();
    build_state(&cache)
}

#[tauri::command]
pub fn set_tool_version(
    id: String,
    version: String,
    cache: State<VersionCache>,
    services: State<Arc<Services>>,
) -> CmdResult<AppState> {
    let tool = tool_by_id(&id)?;
    let mut settings = Settings::load();
    applog::info(format!("{}: version set to {version}", tool.id));
    settings.set_version(tool.id, version);
    settings.save().map_err(|e| format!("Cannot save settings: {e}"))?;

    // Keep PATH in sync when the tool is already there.
    let mut user_path = UserPath::read().map_err(|e| e.to_string())?;
    if user_path.contains_under(&tool.tool_dir(&paths::bin_dir())) {
        apply_path(&mut user_path, tool, true, &settings)?;
        write_path(&user_path, &format!("switched {} to the selected version", tool.id))?;
    }

    // A running service picks up the new version only after a restart.
    match tool.id {
        "apache" | "php" => services.restart("apache"),
        "mysql" => services.restart("mysql"),
        _ => {}
    }
    build_state(&cache)
}

#[tauri::command]
pub fn set_tool_in_path(id: String, enabled: bool, cache: State<VersionCache>) -> CmdResult<AppState> {
    let tool = tool_by_id(&id)?;
    let mut user_path = UserPath::read().map_err(|e| e.to_string())?;
    apply_path(&mut user_path, tool, enabled, &Settings::load())?;
    write_path(&user_path, &format!("{} {}", if enabled { "added" } else { "removed" }, tool.id))?;
    build_state(&cache)
}

/// Adds every installed tool to PATH, or removes all of them.
#[tauri::command]
pub fn set_all_in_path(enabled: bool, cache: State<VersionCache>) -> CmdResult<AppState> {
    let bin = paths::bin_dir();
    let settings = Settings::load();
    let mut user_path = UserPath::read().map_err(|e| e.to_string())?;
    // Reverse so that, after prepending, entries end up in TOOLS order.
    for tool in tools::TOOLS.iter().rev() {
        let enable = enabled && !tool.installs(&bin).is_empty();
        apply_path(&mut user_path, tool, enable, &settings)?;
    }
    write_path(&user_path, if enabled { "added all tools" } else { "removed all tools" })?;
    build_state(&cache)
}

fn service_id(id: &str) -> CmdResult<&'static str> {
    services::SERVICES.iter().map(|(s, _)| *s).find(|s| *s == id).ok_or_else(|| format!("Unknown service: {id}"))
}

#[tauri::command]
pub fn get_services(services: State<Arc<Services>>) -> Vec<ServiceInfo> {
    services.snapshot(&Settings::load())
}

#[tauri::command]
pub fn start_service(id: String, services: State<Arc<Services>>) -> CmdResult<Vec<ServiceInfo>> {
    services.start(service_id(&id)?);
    Ok(services.snapshot(&Settings::load()))
}

#[tauri::command]
pub fn stop_service(id: String, services: State<Arc<Services>>) -> CmdResult<Vec<ServiceInfo>> {
    services.stop(service_id(&id)?);
    Ok(services.snapshot(&Settings::load()))
}

#[tauri::command]
pub fn start_all(services: State<Arc<Services>>) -> Vec<ServiceInfo> {
    for (id, _) in services::SERVICES {
        services.start(id);
    }
    services.snapshot(&Settings::load())
}

#[tauri::command]
pub fn stop_all(services: State<Arc<Services>>) -> Vec<ServiceInfo> {
    for (id, _) in services::SERVICES {
        services.stop(id);
    }
    services.snapshot(&Settings::load())
}

/// Opens a folder of the installation in Explorer, or the local site in the default browser.
#[tauri::command]
pub fn open_location(target: String) -> CmdResult<()> {
    let arg: std::ffi::OsString = match target.as_str() {
        "root" => paths::app_root().into(),
        "www" => paths::www_dir().into(),
        "logs" => paths::logs_dir().into(),
        "config" => paths::config_dir().into(),
        "site" => {
            let port = Settings::load().ports.apache;
            (if port == 80 { "http://localhost/".to_string() } else { format!("http://localhost:{port}/") }).into()
        }
        _ => return Err(format!("Unknown location: {target}")),
    };
    if target != "site" {
        confgen::ensure_layout()?;
    }
    std::process::Command::new("explorer.exe")
        .arg(arg)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_terminal(
    shell: String,
    windows_terminal: bool,
    custom: String,
    cache: State<VersionCache>,
) -> CmdResult<AppState> {
    if !terminal::SHELLS.contains(&shell.as_str()) {
        return Err(format!("Unknown shell: {shell}"));
    }
    let mut settings = Settings::load();
    settings.terminal = settings::Terminal { shell, windows_terminal, custom: custom.trim().to_string() };
    settings.save().map_err(|e| format!("Cannot save settings: {e}"))?;
    build_state(&cache)
}

/// Opens a terminal in www\ with the selected tool versions first in PATH (whether or not they are in
/// the user PATH).
#[tauri::command]
pub fn open_terminal() -> CmdResult<()> {
    let bin = paths::bin_dir();
    let settings = Settings::load();
    let dirs: Vec<terminal::ToolDirs> = tools::TOOLS
        .iter()
        .filter_map(|t| {
            let install = selected_install(t, t.installs(&bin), &settings)?;
            Some(terminal::ToolDirs::new(t, &install.label, &install.dir))
        })
        .collect();
    confgen::ensure_layout()?;
    match terminal::open(&settings.terminal, &dirs, git_dir(&settings).as_deref()) {
        Ok(what) => {
            applog::info(format!("Terminal: opened {what}"));
            Ok(())
        }
        Err(e) => {
            applog::error(format!("Terminal: {e}"));
            Err(e)
        }
    }
}

/// Exits the app (services are stopped on `RunEvent::Exit`).
#[tauri::command]
pub fn quit(app: tauri::AppHandle) {
    app.exit(0);
}
