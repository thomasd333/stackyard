//! Language and theme choice, plus the few strings the Rust side shows itself (the tray menu).
//! The window's strings live in `src/i18n/*.ts`; this module only tells it which locale to use.

use tauri::{Manager, Theme};

use crate::services::Status;

pub const THEMES: [&str; 3] = ["system", "light", "dark"];
/// Locales with a translation; "system" picks one of these from the Windows display language.
pub const LANGUAGES: [&str; 7] = ["en", "de", "es", "fr", "pl", "pt", "uk"];

/// The locale for a `settings.ui.language` value: the choice itself, or the Windows display language
/// when it is "system" (or unknown) and we have that translation, else English.
pub fn resolve(language: &str) -> &'static str {
    LANGUAGES.into_iter().find(|&l| l == language).unwrap_or_else(system_language)
}

fn system_language() -> &'static str {
    // The low 10 bits of a LANGID are the primary language (LANG_* constants in winnt.h).
    let lang = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    match lang & 0x3ff {
        0x07 => "de",
        0x0a => "es",
        0x0c => "fr",
        0x15 => "pl",
        0x16 => "pt",
        0x22 => "uk",
        _ => "en",
    }
}

/// Sets the native title bar (and the webview's `prefers-color-scheme`) to the chosen theme.
pub fn apply_theme(app: &tauri::AppHandle, theme: &str) {
    let theme = match theme {
        "light" => Some(Theme::Light),
        "dark" => Some(Theme::Dark),
        _ => None,
    };
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_theme(theme);
    }
}

/// Tray menu strings. `{name}` and `{port}` are filled in by the caller.
pub struct Tray {
    pub show: &'static str,
    pub start: &'static str,
    pub stop: &'static str,
    pub start_failed: &'static str,
    pub start_all: &'static str,
    pub stop_all: &'static str,
    pub open_site: &'static str,
    pub open_www: &'static str,
    pub open_terminal: &'static str,
    pub quit: &'static str,
    pub stopped: &'static str,
    pub starting: &'static str,
    pub running: &'static str,
    pub stopping: &'static str,
    pub error: &'static str,
}

const EN: Tray = Tray {
    show: "Show Stackyard",
    start: "Start {name}",
    stop: "Stop {name} (port {port})",
    start_failed: "Start {name} (last start failed)",
    start_all: "Start all",
    stop_all: "Stop all",
    open_site: "Open site",
    open_www: "Open www folder",
    open_terminal: "Open terminal",
    quit: "Quit",
    stopped: "Stopped",
    starting: "Starting…",
    running: "Running",
    stopping: "Stopping…",
    error: "Error",
};

const PL: Tray = Tray {
    show: "Pokaż Stackyard",
    start: "Uruchom {name}",
    stop: "Zatrzymaj {name} (port {port})",
    start_failed: "Uruchom {name} (ostatnie uruchomienie nie powiodło się)",
    start_all: "Uruchom wszystkie",
    stop_all: "Zatrzymaj wszystkie",
    open_site: "Otwórz stronę",
    open_www: "Otwórz folder www",
    open_terminal: "Otwórz terminal",
    quit: "Zakończ",
    stopped: "Zatrzymany",
    starting: "Uruchamianie…",
    running: "Działa",
    stopping: "Zatrzymywanie…",
    error: "Błąd",
};

const DE: Tray = Tray {
    show: "Stackyard anzeigen",
    start: "{name} starten",
    stop: "{name} stoppen (Port {port})",
    start_failed: "{name} starten (letzter Start fehlgeschlagen)",
    start_all: "Alle starten",
    stop_all: "Alle stoppen",
    open_site: "Seite öffnen",
    open_www: "Ordner www öffnen",
    open_terminal: "Terminal öffnen",
    quit: "Beenden",
    stopped: "Gestoppt",
    starting: "Wird gestartet…",
    running: "Läuft",
    stopping: "Wird gestoppt…",
    error: "Fehler",
};

const ES: Tray = Tray {
    show: "Mostrar Stackyard",
    start: "Iniciar {name}",
    stop: "Detener {name} (puerto {port})",
    start_failed: "Iniciar {name} (el último inicio falló)",
    start_all: "Iniciar todo",
    stop_all: "Detener todo",
    open_site: "Abrir sitio",
    open_www: "Abrir la carpeta www",
    open_terminal: "Abrir terminal",
    quit: "Salir",
    stopped: "Detenido",
    starting: "Iniciando…",
    running: "En ejecución",
    stopping: "Deteniendo…",
    error: "Error",
};

const FR: Tray = Tray {
    show: "Afficher Stackyard",
    start: "Démarrer {name}",
    stop: "Arrêter {name} (port {port})",
    start_failed: "Démarrer {name} (le dernier démarrage a échoué)",
    start_all: "Tout démarrer",
    stop_all: "Tout arrêter",
    open_site: "Ouvrir le site",
    open_www: "Ouvrir le dossier www",
    open_terminal: "Ouvrir un terminal",
    quit: "Quitter",
    stopped: "Arrêté",
    starting: "Démarrage…",
    running: "En cours",
    stopping: "Arrêt…",
    error: "Erreur",
};

const PT: Tray = Tray {
    show: "Mostrar o Stackyard",
    start: "Iniciar {name}",
    stop: "Parar {name} (porta {port})",
    start_failed: "Iniciar {name} (a última inicialização falhou)",
    start_all: "Iniciar tudo",
    stop_all: "Parar tudo",
    open_site: "Abrir site",
    open_www: "Abrir a pasta www",
    open_terminal: "Abrir terminal",
    quit: "Sair",
    stopped: "Parado",
    starting: "Iniciando…",
    running: "Em execução",
    stopping: "Parando…",
    error: "Erro",
};

const UK: Tray = Tray {
    show: "Показати Stackyard",
    start: "Запустити {name}",
    stop: "Зупинити {name} (порт {port})",
    start_failed: "Запустити {name} (останній запуск не вдався)",
    start_all: "Запустити всі",
    stop_all: "Зупинити всі",
    open_site: "Відкрити сайт",
    open_www: "Відкрити теку www",
    open_terminal: "Відкрити термінал",
    quit: "Вийти",
    stopped: "Зупинено",
    starting: "Запуск…",
    running: "Працює",
    stopping: "Зупинка…",
    error: "Помилка",
};

pub fn tray(locale: &str) -> &'static Tray {
    match locale {
        "de" => &DE,
        "es" => &ES,
        "fr" => &FR,
        "pl" => &PL,
        "pt" => &PT,
        "uk" => &UK,
        _ => &EN,
    }
}

impl Tray {
    pub fn status(&self, status: Status) -> &'static str {
        match status {
            Status::Stopped => self.stopped,
            Status::Starting => self.starting,
            Status::Running => self.running,
            Status::Stopping => self.stopping,
            Status::Error => self.error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_language_has_tray_text_and_a_ui_locale() {
        let index = include_str!("../../src/i18n/index.ts");
        for lang in LANGUAGES {
            if lang != "en" {
                assert!(!std::ptr::eq(tray(lang), &EN), "no tray strings for {lang}");
            }
            assert!(index.contains(&format!("  {lang}: \"")), "{lang} missing from languageNames in index.ts");
        }
    }

    #[test]
    fn resolve_keeps_known_languages() {
        assert_eq!(resolve("en"), "en");
        assert_eq!(resolve("pl"), "pl");
        assert!(LANGUAGES.contains(&resolve("system")));
        assert!(LANGUAGES.contains(&resolve("xx")));
    }
}
