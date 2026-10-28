//! Generates httpd.conf, php.ini and my.ini from embedded templates.
//!
//! Configs are regenerated on every start with absolute paths of the current root, so the whole
//! folder can be moved. User overrides live in `config/<service>/conf.d/`, which is never overwritten.

use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::{self, fwd};
use crate::settings::{Ports, Site};
use crate::sites;

const HTTPD_CONF: &str = include_str!("../templates/httpd.conf");
const PHP_INI: &str = include_str!("../templates/php.ini");
const MY_INI: &str = include_str!("../templates/my.ini");
const WWW_INDEX: &str = include_str!("../templates/index.php");
const ADMINER_PHP: &str = include_str!("../templates/adminer.php");

const APACHE_MODULES: &[&str] = &[
    "access_compat", "actions", "alias", "allowmethods", "auth_basic", "authn_core", "authn_file",
    "authz_core", "authz_host", "authz_user", "autoindex", "deflate", "dir", "env", "expires", "filter",
    "headers", "include", "log_config", "mime", "negotiation", "proxy", "proxy_fcgi", "proxy_http",
    "rewrite", "setenvif", "socache_shmcb", "vhost_alias",
];

const PHP_EXTENSIONS: &[&str] = &[
    "bz2", "curl", "exif", "fileinfo", "gd", "gettext", "intl", "mbstring", "mysqli", "openssl",
    "imap", "pdo_mysql", "pdo_pgsql", "pdo_sqlite", "pgsql", "soap", "sockets", "sodium", "sqlite3", "xsl",
    "zip",
];

/// Microsoft SQL Server drivers; their DLLs carry the PHP version and build in the name,
/// e.g. `php_pdo_sqlsrv_83_ts_x64.dll`.
const PHP_SQLSRV_EXTENSIONS: &[&str] = &["sqlsrv", "pdo_sqlsrv"];

type GenResult<T> = Result<T, String>;

fn io_err(what: &Path) -> impl FnOnce(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", what.display())
}

fn mkdir(dir: &Path) -> GenResult<()> {
    fs::create_dir_all(dir).map_err(io_err(dir))
}

fn write(path: &Path, content: &str) -> GenResult<()> {
    if let Some(parent) = path.parent() {
        mkdir(parent)?;
    }
    fs::write(path, content).map_err(io_err(path))
}

/// Creates the standard folders (www, logs, tmp, data, conf.d dirs) and a start page in an empty www.
pub fn ensure_layout() -> GenResult<()> {
    let config = paths::config_dir();
    for dir in [
        paths::logs_dir(),
        paths::tmp_dir(),
        paths::tmp_dir().join("opcache"),
        paths::data_dir(),
        config.join("apache/conf.d"),
        config.join("php/conf.d"),
        config.join("mysql/conf.d"),
    ] {
        mkdir(&dir)?;
    }
    let www = paths::www_dir();
    if !www.exists() {
        write(&www.join("index.php"), WWW_INDEX)?;
    }
    Ok(())
}

fn fill(template: &str, vars: &[(&str, String)]) -> String {
    let common = [
        ("CONFIG", fwd(&paths::config_dir())),
        ("LOGS", fwd(&paths::logs_dir())),
        ("TMP", fwd(&paths::tmp_dir())),
        ("WWW", fwd(&paths::www_dir())),
    ];
    vars.iter()
        .chain(common.iter())
        .fold(template.to_string(), |out, (key, value)| out.replace(&format!("{{{{{key}}}}}"), value))
}

/// The mod_php DLL of a thread-safe PHP build, with its module name ("php_module" for PHP 8).
fn php_apache_module(php_dir: &Path) -> Option<(PathBuf, String)> {
    fs::read_dir(php_dir).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let major = name.strip_prefix("php")?.strip_suffix("apache2_4.dll")?.to_string();
        let module = if major == "8" { "php_module".to_string() } else { format!("php{major}_module") };
        Some((entry.path(), module))
    })
}

/// DLLs shipped with PHP that also exist in Apache's `bin\` (Apache Lounge's own OpenSSL, or PHP copies
/// Laragon leaves there). Windows looks in the exe's folder first, so unless PHP's copies are loaded
/// by full path beforehand, PHP extensions bind to Apache's (possibly older) versions and fail with
/// "entry point not found". Already loaded DLLs win over the search order.
fn php_preloads(php_dir: &Path, apache_dir: &Path) -> Vec<PathBuf> {
    let apache_bin = apache_dir.join("bin");
    let mut dlls: Vec<PathBuf> = fs::read_dir(php_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            name.ends_with(".dll")
                && !name.ends_with("apache2_4.dll")
                && !is_vc_runtime(&name)
                && apache_bin.join(&name).is_file()
        })
        .collect();
    dlls.sort();
    dependency_order(dlls)
}

/// The Visual C++ runtime (vcruntime140*.dll, msvcp140*.dll), which release zips put next to Apache, PHP
/// and MySQL so they run without the redistributable. httpd.exe has already loaded Apache's copy, and
/// newer runtimes serve older builds, so preloading PHP's copy would only add a second one.
fn is_vc_runtime(name: &str) -> bool {
    ["vcruntime", "msvcp", "concrt", "vccorlib"].iter().any(|p| name.starts_with(p))
}

/// Orders DLLs so each comes after the DLLs (from the same set) it imports. Otherwise e.g. libpq.dll,
/// loaded before libssl, would pull Apache's libssl in through its own import.
fn dependency_order(dlls: Vec<PathBuf>) -> Vec<PathBuf> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let names: Vec<String> = dlls.iter().map(|p| name(p)).collect();
    let imports: Vec<Vec<usize>> = dlls
        .iter()
        .map(|p| {
            pe_imports(p)
                .iter()
                .filter_map(|imp| names.iter().position(|n| n.eq_ignore_ascii_case(imp)))
                .collect()
        })
        .collect();

    fn visit(i: usize, imports: &[Vec<usize>], state: &mut [u8], out: &mut Vec<usize>) {
        // 0 = new, 1 = in progress (cycle guard), 2 = done
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &dep in &imports[i] {
            visit(dep, imports, state, out);
        }
        state[i] = 2;
        out.push(i);
    }

    let mut state = vec![0u8; dlls.len()];
    let mut order = Vec::with_capacity(dlls.len());
    for i in 0..dlls.len() {
        visit(i, &imports, &mut state, &mut order);
    }
    order.into_iter().map(|i| dlls[i].clone()).collect()
}

/// Names of the DLLs a 64-bit PE file imports (empty if it can't be parsed).
fn pe_imports(path: &Path) -> Vec<String> {
    fn parse(data: &[u8]) -> Option<Vec<String>> {
        let u16_at = |o: usize| data.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
        let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));

        let pe = u32_at(0x3C)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let coff = pe + 4;
        let sections = u16_at(coff + 2)? as usize;
        let optional = coff + 20;
        let optional_size = u16_at(coff + 16)? as usize;
        if u16_at(optional)? != 0x20B {
            return None; // not PE32+
        }
        let import_rva = u32_at(optional + 120)?; // data directory #1 (imports)

        // Maps an RVA to a file offset using the section table.
        let table = optional + optional_size;
        let to_offset = |rva: u32| -> Option<usize> {
            (0..sections).find_map(|s| {
                let sec = table + s * 40;
                let (size, va, raw) = (u32_at(sec + 8)?, u32_at(sec + 12)?, u32_at(sec + 20)?);
                (rva >= va && rva < va + size.max(1)).then(|| (rva - va + raw) as usize)
            })
        };

        let mut names = Vec::new();
        let mut desc = to_offset(import_rva)?;
        loop {
            let name_rva = u32_at(desc + 12)?;
            if name_rva == 0 {
                break;
            }
            let start = to_offset(name_rva)?;
            let end = start + data.get(start..)?.iter().position(|&b| b == 0)?;
            names.push(String::from_utf8_lossy(&data[start..end]).into_owned());
            desc += 20;
        }
        Some(names)
    }
    fs::read(path).ok().and_then(|data| parse(&data)).unwrap_or_default()
}

/// Adminer, `bin\adminer\adminer-<version>.php` (the newest when there are several).
pub fn adminer_file() -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(paths::bin_dir().join("adminer"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            name.starts_with("adminer") && name.ends_with(".php")
        })
        .collect();
    files.sort_by(|a, b| crate::tools::natural_cmp(&b.to_string_lossy(), &a.to_string_lossy()));
    files.into_iter().next()
}

/// Writes `config\adminer\index.php` and returns the Apache block that serves it at /adminer/ to this
/// computer only, or a comment when Adminer (or PHP) is missing.
fn write_adminer(ports: &Ports, has_php: bool) -> GenResult<String> {
    let Some(adminer) = adminer_file().filter(|_| has_php) else {
        return Ok("# Adminer is not installed (bin/adminer/adminer-*.php)".into());
    };
    let dir = paths::config_dir().join("adminer");
    let index = fill(
        ADMINER_PHP,
        &[
            ("ADMINER", fwd(&adminer)),
            ("MYSQL_PORT", ports.mysql.to_string()),
            ("POSTGRESQL_PORT", ports.postgresql.to_string()),
        ],
    );
    write(&dir.join("index.php"), &index)?;
    Ok(format!(
        "Alias /adminer \"{dir}\"\n<Directory \"{dir}\">\n    Require local\n    DirectoryIndex index.php\n</Directory>",
        dir = fwd(&dir)
    ))
}

pub fn write_apache(apache_dir: &Path, php_dir: Option<&Path>, ports: &Ports, sites: &[Site]) -> GenResult<PathBuf> {
    let port = ports.apache;
    let modules_dir = apache_dir.join("modules");
    let preloads = php_dir.map(|dir| php_preloads(dir, apache_dir)).unwrap_or_default();
    let modules: Vec<String> = preloads
        .iter()
        .map(|dll| format!("LoadFile \"{}\"", fwd(dll)))
        .chain(
            APACHE_MODULES
                .iter()
                .filter(|m| modules_dir.join(format!("mod_{m}.so")).is_file())
                .map(|m| format!("LoadModule {m}_module modules/mod_{m}.so")),
        )
        .collect();

    let php = match php_dir {
        None => "# PHP is not installed".to_string(),
        Some(dir) => {
            let (dll, module) = php_apache_module(dir).ok_or_else(|| {
                format!("{} has no php*apache2_4.dll - Apache needs a Thread Safe (TS) PHP build", dir.display())
            })?;
            let ini = write_php(dir)?;
            format!(
                "LoadModule {module} \"{}\"\nPHPIniDir \"{}\"\n<FilesMatch \"\\.php$\">\n    SetHandler application/x-httpd-php\n</FilesMatch>",
                fwd(&dll),
                fwd(ini.parent().unwrap_or(&ini)),
            )
        }
    };

    let conf = fill(
        HTTPD_CONF,
        &[
            ("APACHE_DIR", fwd(apache_dir)),
            ("PORT", port.to_string()),
            ("MODULES", modules.join("\n")),
            ("PHP", php),
            ("SITES", sites::apache_vhosts(sites, port)),
            ("ADMINER", write_adminer(ports, php_dir.is_some())?),
        ],
    );
    let path = paths::config_dir().join("apache/httpd.conf");
    write(&path, &conf)?;
    Ok(path)
}

/// File names of the SQL Server driver DLLs built for this PHP (same major.minor, x64, TS or NTS),
/// e.g. `php_pdo_sqlsrv_83_ts_x64.dll` for `bin\php\php-8.3.33-Win32-vs16-x64`.
pub(crate) fn sqlsrv_extensions(php_dir: &Path) -> Vec<String> {
    let label = php_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let Some(version) = major_minor(&label) else { return Vec::new() };
    let thread_safe = fs::read_dir(php_dir).into_iter().flatten().flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_lowercase();
        name.starts_with("php") && name.ends_with("ts.dll")
    });
    let build = if thread_safe { "ts" } else { "nts" };
    let ext_dir = php_dir.join("ext");

    PHP_SQLSRV_EXTENSIONS
        .iter()
        .map(|base| format!("php_{base}_{version}_{build}_x64.dll"))
        .filter(|dll| ext_dir.join(dll).is_file())
        .collect()
}

/// "php-8.3.33-Win32-vs16-x64" -> "83"
fn major_minor(label: &str) -> Option<String> {
    let version = label.split(|c: char| !c.is_ascii_digit() && c != '.').find(|s| s.contains('.'))?;
    let parts: Vec<&str> = version.split('.').take(2).collect();
    (parts.len() == 2).then(|| parts.concat())
}

/// Writes php.ini to `config/php/` (used by Apache) and into the PHP folder itself, so the CLI
/// from PATH uses the same settings. The PHP folder's original php.ini is kept as php.ini.orig.
pub fn write_php(php_dir: &Path) -> GenResult<PathBuf> {
    let ext_dir = php_dir.join("ext");
    let mut extensions: Vec<String> = PHP_EXTENSIONS
        .iter()
        .filter(|e| ext_dir.join(format!("php_{e}.dll")).is_file())
        .map(|e| format!("extension = {e}"))
        .collect();
    extensions.extend(sqlsrv_extensions(php_dir).into_iter().map(|dll| format!("extension = {dll}")));
    if ext_dir.join("php_opcache.dll").is_file() {
        extensions.push("zend_extension = opcache".into());
    }

    let mut ini = fill(PHP_INI, &[("PHP_DIR", fwd(php_dir)), ("EXTENSIONS", extensions.join("\n"))]);

    // PHP has no include directive, so user overrides are appended (later values win).
    let conf_d = paths::config_dir().join("php/conf.d");
    let mut overrides: Vec<PathBuf> = fs::read_dir(&conf_d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ini")))
        .collect();
    overrides.sort();
    for file in overrides {
        let content = fs::read_to_string(&file).map_err(io_err(&file))?;
        ini.push_str(&format!("\n; ---- {} ----\n[PHP]\n{content}\n", fwd(&file)));
    }

    let path = paths::config_dir().join("php/php.ini");
    write(&path, &ini)?;

    let cli_ini = php_dir.join("php.ini");
    let orig = php_dir.join("php.ini.orig");
    if cli_ini.is_file() && !orig.exists() && !is_generated(&cli_ini) {
        fs::rename(&cli_ini, &orig).map_err(io_err(&cli_ini))?;
    }
    write(&cli_ini, &ini)?;
    Ok(path)
}

fn is_generated(file: &Path) -> bool {
    fs::read_to_string(file).is_ok_and(|s| s.starts_with("; Generated by Stackyard"))
}

/// Data directory per MySQL major.minor (e.g. `data/mysql-8.4`): data files are not portable
/// across major versions.
pub fn mysql_datadir(version_label: &str) -> PathBuf {
    let version: Vec<&str> = version_label
        .split(|c: char| !c.is_ascii_digit() && c != '.')
        .find(|s| s.contains('.'))
        .map(|s| s.split('.').take(2).collect())
        .unwrap_or_default();
    let suffix = if version.len() == 2 { version.join(".") } else { version_label.to_string() };
    paths::data_dir().join(format!("mysql-{suffix}"))
}

pub fn write_mysql(mysql_dir: &Path, datadir: &Path, port: u16) -> GenResult<PathBuf> {
    let ini = fill(
        MY_INI,
        &[("MYSQL_DIR", fwd(mysql_dir)), ("DATADIR", fwd(datadir)), ("PORT", port.to_string())],
    );
    let path = paths::config_dir().join("mysql/my.ini");
    write(&path, &ini)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datadir_name() {
        assert!(mysql_datadir("mysql-8.4.3-winx64").ends_with("mysql-8.4"));
        assert!(mysql_datadir("default").ends_with("mysql-default"));
    }

    #[test]
    fn php_major_minor() {
        assert_eq!(major_minor("php-8.3.33-Win32-vs16-x64").as_deref(), Some("83"));
        assert_eq!(major_minor("php-8.5.12-1-Win32-vs17-x64").as_deref(), Some("85"));
        assert_eq!(major_minor("default"), None);
    }

    #[test]
    fn reads_pe_imports() {
        // Any 64-bit exe works; our own test binary imports kernel32.
        let exe = std::env::current_exe().unwrap();
        assert!(pe_imports(&exe).iter().any(|n| n.eq_ignore_ascii_case("kernel32.dll")));
    }

    #[test]
    fn placeholders_filled() {
        assert_eq!(fill("a {{X}} b", &[("X", "1".into())]), "a 1 b");
    }
}
