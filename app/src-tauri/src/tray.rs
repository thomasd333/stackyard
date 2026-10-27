//! System tray icon: quick service control while the window is hidden.
//!
//! Tray strings are English-only for now; move them to a shared locale when translations arrive.

use std::sync::Arc;
use std::time::Duration;

use tauri::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

use crate::commands;
use crate::services::{SERVICES, ServiceInfo, Services, Status};
use crate::settings::Settings;

const SVC_PREFIX: &str = "svc:";
const OPEN_PREFIX: &str = "open:";

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    let item = |id: &str, text: &str| MenuItem::with_id(app, id, text, true, None::<&str>);
    let separator = || PredefinedMenuItem::separator(app);

    let service_items: Vec<MenuItem<_>> = SERVICES
        .iter()
        .map(|(id, name)| item(&format!("{SVC_PREFIX}{id}"), &format!("Start {name}")))
        .collect::<Result<_, _>>()?;

    let menu = Menu::new(app)?;
    menu.append(&item("show", "Show Stackyard")?)?;
    menu.append(&separator()?)?;
    for service_item in &service_items {
        menu.append(service_item)?;
    }
    menu.append(&item("start_all", "Start all")?)?;
    menu.append(&item("stop_all", "Stop all")?)?;
    menu.append(&separator()?)?;
    menu.append(&item(&format!("{OPEN_PREFIX}site"), "Open site")?)?;
    menu.append(&item(&format!("{OPEN_PREFIX}www"), "Open www folder")?)?;
    menu.append(&item("terminal", "Open terminal")?)?;
    menu.append(&separator()?)?;
    menu.append(&item("quit", "Quit")?)?;

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
    std::thread::spawn(move || watch_services(app, tray, service_items));
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

/// Keeps menu labels and the tooltip in sync with service status.
fn watch_services(app: AppHandle, tray: TrayIcon, items: Vec<MenuItem<tauri::Wry>>) {
    let mut last: Vec<ServiceInfo> = Vec::new();
    loop {
        let current = app.state::<Arc<Services>>().snapshot(&Settings::load());
        if current != last {
            for (svc, item) in current.iter().zip(&items) {
                let (text, enabled) = match svc.status {
                    Status::Running => (format!("Stop {} (port {})", svc.name, svc.port), true),
                    Status::Starting | Status::Stopping => (format!("{}: {}", svc.name, svc.status.label()), false),
                    Status::Error => (format!("Start {} (last start failed)", svc.name), svc.version.is_some()),
                    Status::Stopped => (format!("Start {}", svc.name), svc.version.is_some()),
                };
                let _ = item.set_text(text);
                let _ = item.set_enabled(enabled);
            }
            let lines: Vec<String> = current.iter().map(|s| format!("{}: {}", s.name, s.status.label())).collect();
            let _ = tray.set_tooltip(Some(format!("Stackyard\n{}", lines.join("\n"))));
            last = current;
        }
        std::thread::sleep(Duration::from_millis(700));
    }
}
