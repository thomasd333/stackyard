//! System tray icon: quick service control while the window is hidden.

use std::sync::Arc;
use std::time::Duration;

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::commands;
use crate::i18n::{self, Tray};
use crate::services::{SERVICES, ServiceInfo, Services, Status};
use crate::settings::{Settings, Site};
use crate::sites;

const SVC_PREFIX: &str = "svc:";
const OPEN_PREFIX: &str = "open:";

/// Picks one string out of `Tray`.
type Text = fn(&Tray) -> &'static str;

/// Menu items with fixed text, relabelled when the language changes.
const FIXED: [(&str, Text); 7] = [
    ("show", |t| t.show),
    ("start_all", |t| t.start_all),
    ("stop_all", |t| t.stop_all),
    ("open:www", |t| t.open_www),
    ("open:adminer", |t| t.open_adminer),
    ("terminal", |t| t.open_terminal),
    ("quit", |t| t.quit),
];

struct Items {
    services: Vec<MenuItem<tauri::Wry>>,
    fixed: Vec<(MenuItem<tauri::Wry>, Text)>,
    sites: Submenu<tauri::Wry>,
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str| MenuItem::with_id(app, id, "", true, None::<&str>);
    let separator = || PredefinedMenuItem::separator(app);

    let services: Vec<MenuItem<_>> =
        SERVICES.iter().map(|(id, _)| item(&format!("{SVC_PREFIX}{id}"))).collect::<Result<_, _>>()?;
    let fixed: Vec<_> = FIXED.iter().map(|&(id, text)| Ok((item(id)?, text))).collect::<tauri::Result<_>>()?;
    let get = |id: &str| &fixed[FIXED.iter().position(|(x, _)| *x == id).unwrap()].0;
    let sites = Submenu::new(app, "", true)?;

    let menu = Menu::new(app)?;
    menu.append(get("show"))?;
    menu.append(&separator()?)?;
    for service_item in &services {
        menu.append(service_item)?;
    }
    menu.append(get("start_all"))?;
    menu.append(get("stop_all"))?;
    menu.append(&separator()?)?;
    menu.append(&sites)?;
    menu.append(get("open:www"))?;
    if crate::confgen::adminer_file().is_some() {
        menu.append(get("open:adminer"))?;
    }
    menu.append(get("terminal"))?;
    menu.append(&separator()?)?;
    menu.append(get("quit"))?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Stackyard")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_event);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder.build(app)?;

    let app = app.clone();
    let items = Items { services, fixed, sites };
    std::thread::spawn(move || watch_services(app, tray, items));
    Ok(())
}

/// Fills the "Open site" submenu: the main site (www\) and every virtual host.
fn fill_sites(app: &AppHandle, submenu: &Submenu<tauri::Wry>, sites: &[Site]) -> tauri::Result<()> {
    for old in submenu.items()? {
        submenu.remove(&old)?;
    }
    let entries = std::iter::once(("site".to_string(), sites::TLD.to_string()))
        .chain(sites.iter().map(|s| (format!("site:{}", s.name), format!("{}.{}", s.name, sites::TLD))));
    for (target, text) in entries {
        submenu.append(&MenuItem::with_id(app, format!("{OPEN_PREFIX}{target}"), text, true, None::<&str>)?)?;
    }
    Ok(())
}

pub fn show_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn on_tray_event(tray: &TrayIcon, event: TrayIconEvent) {
    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
        show_window(tray.app_handle());
    }
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let services = app.state::<Arc<Services>>();
    let id = event.id().as_ref();
    if let Some(service) = id.strip_prefix(SVC_PREFIX) {
        let Some(&(service, _)) = SERVICES.iter().find(|(s, _)| *s == service) else { return };
        let running = services
            .snapshot(&Settings::load())
            .iter()
            .any(|s| s.id == service && s.status == Status::Running);
        if running { services.stop(service) } else { services.start(service) }
    } else if let Some(target) = id.strip_prefix(OPEN_PREFIX) {
        let _ = commands::open_location(target.to_string());
    } else {
        match id {
            "show" => show_window(app),
            // Failures are logged by the command; the window may be hidden, so there's nowhere to show them.
            "terminal" => {
                let _ = commands::open_terminal();
            }
            "start_all" => SERVICES.iter().for_each(|(s, _)| services.start(s)),
            "stop_all" => SERVICES.iter().for_each(|(s, _)| services.stop(s)),
            // RunEvent::Exit stops the services before the process ends.
            "quit" => app.exit(0),
            _ => {}
        }
    }
}

/// Keeps menu labels, the site list and the tooltip in sync with the services and settings.
fn watch_services(app: AppHandle, tray: TrayIcon, items: Items) {
    let mut last: Vec<ServiceInfo> = Vec::new();
    let mut last_sites: Option<Vec<String>> = None;
    let mut last_locale = "";
    loop {
        let settings = Settings::load();
        let locale = i18n::resolve(&settings.ui.language);
        let text = i18n::tray(locale);
        let relabel = locale != last_locale;
        if relabel {
            for (item, get) in &items.fixed {
                let _ = item.set_text(get(text));
            }
            let _ = items.sites.set_text(text.open_site);
            last_locale = locale;
        }
        let site_names: Vec<String> = settings.sites.iter().map(|s| s.name.clone()).collect();
        if last_sites.as_ref() != Some(&site_names) && fill_sites(&app, &items.sites, &settings.sites).is_ok() {
            last_sites = Some(site_names);
        }
        let current = app.state::<Arc<Services>>().snapshot(&settings);
        if relabel || current != last {
            for (svc, item) in current.iter().zip(&items.services) {
                let fill = |s: &str| s.replace("{name}", svc.name).replace("{port}", &svc.port.to_string());
                let (label, enabled) = match svc.status {
                    Status::Running => (fill(text.stop), true),
                    Status::Starting | Status::Stopping => (format!("{}: {}", svc.name, text.status(svc.status)), false),
                    Status::Error => (fill(text.start_failed), svc.version.is_some()),
                    Status::Stopped => (fill(text.start), svc.version.is_some()),
                };
                let _ = item.set_text(label);
                let _ = item.set_enabled(enabled);
            }
            let lines: Vec<String> =
                current.iter().map(|s| format!("{}: {}", s.name, text.status(s.status))).collect();
            let _ = tray.set_tooltip(Some(format!("Stackyard\n{}", lines.join("\n"))));
            last = current;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}
