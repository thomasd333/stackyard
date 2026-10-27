use std::path::PathBuf;

/// Root directory of the portable installation (the folder that contains `bin/`).
///
/// Resolution order:
/// 1. `STACKYARD_ROOT` environment variable (handy for testing),
/// 2. debug builds: two levels above `src-tauri` (i.e. the repo's parent, `D:\SERVER`),
/// 3. release builds: the directory containing the executable.
pub fn app_root() -> PathBuf {
    if let Some(root) = std::env::var_os("STACKYARD_ROOT") {
        return PathBuf::from(root);
    }

    if cfg!(debug_assertions) {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if let Some(root) = manifest.parent().and_then(|p| p.parent()) {
            return root.to_path_buf();
        }
    }

    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn bin_dir() -> PathBuf {
    app_root().join("bin")
}

/// All app-owned configuration lives here, never in %APPDATA%.
pub fn config_dir() -> PathBuf {
    app_root().join("config")
}

/// Apache document root.
pub fn www_dir() -> PathBuf {
    app_root().join("www")
}

pub fn logs_dir() -> PathBuf {
    app_root().join("logs")
}

pub fn tmp_dir() -> PathBuf {
    app_root().join("tmp")
}

/// Database data directories.
pub fn data_dir() -> PathBuf {
    app_root().join("data")
}

/// PHP and MySQL read paths from their config files in the ANSI code page, so a root with non-ASCII
/// characters (e.g. "D:\Mój serwer") breaks extension loading and MySQL startup. Spaces are fine.
pub fn root_is_supported() -> bool {
    app_root().to_string_lossy().is_ascii()
}

pub const UNSUPPORTED_ROOT_MESSAGE: &str = "The Stackyard folder path contains non-English characters (such as ą, ó, ż). \
PHP and MySQL cannot work with such paths. Move Stackyard to a folder like D:\\Stackyard.";

/// Path with forward slashes, as Apache, PHP and MySQL configs prefer.
pub fn fwd(path: &std::path::Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
