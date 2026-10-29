use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::State;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::applog;
use crate::autostart;
use crate::confgen;
use crate::downloads::{Downloads, Job, Updates};
use crate::env_path::{self, UserPath};
use crate::i18n;
use crate::paths;
use crate::services::{self, ServiceInfo, Services};
use crate::settings::{self, Settings};
use crate::sites;
use crate::terminal;
use crate::tools::{self, Install, ToolDef};

/// Version strings are cached per executable dir: spawning `php -v` etc. on every refresh is wasteful.
#[derive(Default)]
pub struct VersionCache(Mutex<HashMap<PathBuf, Option<String>>>);

type CmdResult<T> = Result<T, String>;

/// "Chip in a coffee" link in Settings > About.
const SUPPORT_URL: &str = "https://ko-fi.com/thomasd333";

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
    sites: Vec<SiteInfo>,
    /// Folders offered when adding a site (see `sites::suggestions`).
    site_suggestions: Vec<String>,
    /// Address of the default host (localhost -> www).
    main_url: String,
    ui: UiInfo,
    ports: settings::Ports,
    /// Where Adminer is served when it is installed (bin\adminer); it needs Apache running.
    adminer_url: Option<String>,
    startup: StartupInfo,
    /// Stackyard's own version, shown in the header.
    version: &'static str,
    /// Parse error of a settings.json that was set aside and replaced (shown once).
    settings_recovered: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupInfo {
    with_windows: bool,
    restore_services: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UiInfo {
    theme: String,
    language: String,
    /// The locale to show: `language`, or the one "system" resolves to.
    locale: &'static str,
    /// What "system" resolves to, shown next to that option.
    system_locale: &'static str,
}

#[derive(Serialize)]
pub struct SiteInfo {
    name: String,
    host: String,
    root: String,
    url: String,
    exists: bool,
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
        sites: settings
            .sites
            .iter()
            .map(|s| SiteInfo {
                host: format!("{}.{}", s.name, sites::TLD),
                url: sites::url(Some(&s.name), settings.ports.apache),
                exists: sites::resolve_root(&s.root).is_dir(),
                name: s.name.clone(),
                root: s.root.clone(),
            })
            .collect(),
        site_suggestions: sites::suggestions(),
        main_url: sites::url(None, settings.ports.apache),
        ports: settings::Ports {
            apache: settings.ports.apache,
            mysql: settings.ports.mysql,
            postgresql: settings.ports.postgresql,
        },
        adminer_url: confgen::adminer_file().map(|_| adminer_url(settings.ports.apache)),
        startup: StartupInfo {
            with_windows: settings.startup.with_windows,
            restore_services: settings.startup.restore_services,
        },
        version: env!("CARGO_PKG_VERSION"),
        settings_recovered: settings::take_recovery(),
        ui: UiInfo {
            locale: i18n::resolve(&settings.ui.language),
            system_locale: i18n::resolve("system"),
            theme: settings.ui.theme.clone(),
            language: settings.ui.language.clone(),
        },
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

/// Opens a folder of the installation in Explorer, or a URL (local site, Adminer, support page) in the
/// default browser.
#[tauri::command]
pub fn open_location(target: String) -> CmdResult<()> {
    let arg: std::ffi::OsString = match target.as_str() {
        "root" => paths::app_root().into(),
        "www" => paths::www_dir().into(),
        "logs" => paths::logs_dir().into(),
        "config" => paths::config_dir().into(),
        "site" => sites::url(None, Settings::load().ports.apache).into(),
        "adminer" => adminer_url(Settings::load().ports.apache).into(),
        "support" => SUPPORT_URL.into(),
        // "site:<name>" opens a virtual host.
        _ => match target.strip_prefix("site:") {
            Some(name) => sites::url(Some(&sites::normalize_name(name)?), Settings::load().ports.apache).into(),
            None => return Err(format!("Unknown location: {target}")),
        },
    };
    if matches!(target.as_str(), "root" | "www" | "logs" | "config") {
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

/// Saves the startup options and adds or removes the Windows sign-in entry.
#[tauri::command]
pub fn set_startup(with_windows: bool, restore_services: bool, cache: State<VersionCache>) -> CmdResult<AppState> {
    autostart::set(with_windows).map_err(|e| format!("Cannot change the Windows startup entry: {e}"))?;
    let mut settings = Settings::load();
    settings.startup = settings::Startup { with_windows, restore_services };
    settings.save().map_err(|e| format!("Cannot save settings: {e}"))?;
    applog::info(format!("Startup: with Windows {with_windows}, restore services {restore_services}"));
    build_state(&cache)
}

/// Saves the ports; a running service restarts on its new port.
#[tauri::command]
pub fn set_ports(
    apache: u16,
    mysql: u16,
    postgresql: u16,
    cache: State<VersionCache>,
    services: State<Arc<Services>>,
) -> CmdResult<AppState> {
    if apache == 0 || mysql == 0 || postgresql == 0 {
        return Err("Ports must be between 1 and 65535.".into());
    }
    if apache == mysql || apache == postgresql || mysql == postgresql {
        return Err("Apache, MySQL and PostgreSQL need different ports.".into());
    }
    let mut settings = Settings::load();
    let old = settings::Ports { ..settings.ports };
    settings.ports = settings::Ports { apache, mysql, postgresql };
    settings.save().map_err(|e| format!("Cannot save settings: {e}"))?;
    applog::info(format!("Ports set to Apache {apache}, MySQL {mysql}, PostgreSQL {postgresql}"));
    // Apache also restarts for a database port: Adminer's defaults are written when it starts.
    if (old.apache, old.mysql, old.postgresql) != (apache, mysql, postgresql) {
        services.restart("apache");
    }
    if old.mysql != mysql {
        services.restart("mysql");
    }
    if old.postgresql != postgresql {
        services.restart("postgresql");
    }
    build_state(&cache)
}

fn adminer_url(apache_port: u16) -> String {
    format!("{}adminer/", sites::url(None, apache_port))
}

/// Saves the theme and language; the tray picks the language up on its next refresh.
#[tauri::command]
pub fn set_ui(app: tauri::AppHandle, theme: String, language: String, cache: State<VersionCache>) -> CmdResult<AppState> {
    if !i18n::THEMES.contains(&theme.as_str()) {
        return Err(format!("Unknown theme: {theme}"));
    }
    if language != "system" && !i18n::LANGUAGES.contains(&language.as_str()) {
        return Err(format!("Unknown language: {language}"));
    }
    let mut settings = Settings::load();
    i18n::apply_theme(&app, &theme);
    settings.ui = settings::Ui { theme, language };
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

/// Adds a `<name>.localhost` site; a running Apache restarts to pick it up.
#[tauri::command]
pub fn add_site(
    name: String,
    root: String,
    cache: State<VersionCache>,
    services: State<Arc<Services>>,
) -> CmdResult<AppState> {
    let name = sites::normalize_name(&name)?;
    let root = sites::normalize_root(&root)?;
    let mut settings = Settings::load();
    if settings.sites.iter().any(|s| s.name == name) {
        return Err(format!("Site {name}.{} already exists.", sites::TLD));
    }
    applog::info(format!("Sites: added {name}.{} -> {root}", sites::TLD));
    settings.sites.push(settings::Site { name, root });
    settings.save().map_err(|e| format!("Cannot save settings: {e}"))?;
    services.restart("apache");
    build_state(&cache)
}

/// Removes a site from the Apache config; its files are left alone.
#[tauri::command]
pub fn remove_site(name: String, cache: State<VersionCache>, services: State<Arc<Services>>) -> CmdResult<AppState> {
    let mut settings = Settings::load();
    let before = settings.sites.len();
    settings.sites.retain(|s| s.name != name);
    if settings.sites.len() != before {
        applog::info(format!("Sites: removed {name}.{}", sites::TLD));
        settings.save().map_err(|e| format!("Cannot save settings: {e}"))?;
        services.restart("apache");
    }
    build_state(&cache)
}

/// Fetches the versions available for download (network, runs off the main thread).
#[tauri::command]
pub async fn check_updates(downloads: State<'_, Arc<Downloads>>) -> CmdResult<Updates> {
    let downloads = Arc::clone(&downloads);
    tauri::async_runtime::spawn_blocking(move || downloads.check()).await.map_err(|e| e.to_string())
}

/// Offers from the last check against what is installed now (no network); None before the first check.
#[tauri::command]
pub async fn get_updates(downloads: State<'_, Arc<Downloads>>) -> CmdResult<Option<Updates>> {
    let downloads = Arc::clone(&downloads);
    tauri::async_runtime::spawn_blocking(move || downloads.updates()).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn download_tool(id: String, version: String, downloads: State<Arc<Downloads>>) -> CmdResult<Option<Job>> {
    downloads.start(&id, &version)?;
    Ok(downloads.job())
}

#[tauri::command]
pub fn get_download(downloads: State<Arc<Downloads>>) -> Option<Job> {
    downloads.job()
}

#[tauri::command]
pub fn dismiss_download(downloads: State<Arc<Downloads>>) {
    downloads.dismiss();
}

/// Grows the window so `content_height` (CSS px) fits without scrolling, but never past the screen's
/// work area (taskbar excluded); moves it up when the bottom would end up off screen.
#[tauri::command]
pub fn fit_window(window: tauri::WebviewWindow, content_height: f64) -> CmdResult<()> {
    let err = |e: tauri::Error| e.to_string();
    let Some(monitor) = window.current_monitor().map_err(err)? else { return Ok(()) };
    let inner = window.inner_size().map_err(err)?;
    let outer = window.outer_size().map_err(err)?;
    let chrome = outer.height.saturating_sub(inner.height);
    let area = monitor.work_area();

    let wanted = (content_height * window.scale_factor().map_err(err)?).ceil() as u32;
    let height = wanted.min(area.size.height.saturating_sub(chrome));
    if height <= inner.height {
        return Ok(());
    }
    window.set_size(tauri::PhysicalSize::new(inner.width, height)).map_err(err)?;

    let pos = window.outer_position().map_err(err)?;
    let bottom = area.position.y + area.size.height as i32;
    let outer_height = (height + chrome) as i32;
    if pos.y + outer_height > bottom {
        let y = (bottom - outer_height).max(area.position.y);
        window.set_position(tauri::PhysicalPosition::new(pos.x, y)).map_err(err)?;
    }
    Ok(())
}

/// Exits the app (services are stopped on `RunEvent::Exit`).
#[tauri::command]
pub fn quit(app: tauri::AppHandle) {
    app.exit(0);
}
