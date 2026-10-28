mod applog;
mod commands;
mod confgen;
mod downloads;
mod env_path;
mod paths;
mod ports;
mod process;
mod relocate;
mod services;
mod settings;
mod terminal;
mod tools;
mod tray;

use std::sync::Arc;

use tauri::{Manager, RunEvent, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    applog::init();
    settings::Settings::ensure_file();
    relocate::on_startup();

    // Child processes inherit the error mode: a DLL problem in httpd/mysqld must fail with an exit code
    // we can report, not pop up a modal "entry point not found" box that blocks the start.
    unsafe {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode,
        };
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }

    let app = tauri::Builder::default()
        // Must be first: a second launch just brings the running instance to front.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| tray::show_window(app)))
        .manage(commands::VersionCache::default())
        .manage(Arc::new(services::Services::default()))
        .manage(Arc::new(downloads::Downloads::default()))
        .setup(|app| {
            tray::setup(app.handle())?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps Stackyard (and its services) running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_state,
            commands::refresh,
            commands::set_tool_version,
            commands::set_tool_in_path,
            commands::set_all_in_path,
            commands::get_services,
            commands::start_service,
            commands::stop_service,
            commands::start_all,
            commands::stop_all,
            commands::open_location,
            commands::set_terminal,
            commands::open_terminal,
            commands::check_updates,
            commands::get_updates,
            commands::download_tool,
            commands::get_download,
            commands::dismiss_download,
            commands::fit_window,
            commands::quit,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        // Stop services cleanly; the Job Objects are the safety net if this never runs.
        if let RunEvent::Exit = event {
            applog::info("Exiting, stopping services");
            handle.state::<Arc<services::Services>>().stop_all_blocking();
        }
    });
}
