//! "Start with Windows": a value under HKCU\...\Run, which needs no admin rights.
//!
//! Stackyard is portable, so the value is rewritten at every launch while the option is on: after the
//! folder is moved it points at the new exe.

use std::io;

use winreg::RegKey;
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

use crate::applog;
use crate::settings::Settings;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "Stackyard";
/// Passed by the Run entry: start hidden in the tray instead of showing the window.
pub const ARG: &str = "--autostart";

fn command() -> io::Result<String> {
    Ok(format!("\"{}\" {ARG}", std::env::current_exe()?.display()))
}

fn ours(value: &str) -> bool {
    command().is_ok_and(|c| c.eq_ignore_ascii_case(value))
}

pub fn set(enabled: bool) -> io::Result<()> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_WRITE)?;
    if enabled {
        key.set_value(VALUE, &command()?)
    } else {
        // Leave an entry of another Stackyard copy alone.
        match key.get_value::<String, _>(VALUE) {
            Ok(v) if ours(&v) => key.delete_value(VALUE),
            _ => Ok(()),
        }
    }
}

/// Keeps the Run entry in line with the setting (and with the exe's current location).
pub fn sync(settings: &Settings) {
    if let Err(e) = set(settings.startup.with_windows) {
        applog::warn(format!("Cannot update the Windows startup entry: {e}"));
    }
}

/// Whether this process was started by the Run entry.
pub fn launched_at_sign_in() -> bool {
    std::env::args().any(|a| a == ARG)
}
