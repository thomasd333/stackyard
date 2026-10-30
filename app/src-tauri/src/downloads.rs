//! Downloads new versions of PHP, Node.js and Composer from their official sources, and checks
//! whether a newer Stackyard is out (only checks: the user downloads it from the releases page).
//!
//! A new PHP or Node version is installed next to the existing ones (`bin\<tool>\<version-dir>\`) and
//! picked with the normal version switch, so going back is one click. Composer is a single file: a flat
//! install (`bin\composer\composer.phar`) is replaced in place and the old file kept as
//! `composer-<version>.phar`.
//!
//! Uses Windows' own curl.exe, tar.exe and certutil.exe (Windows 10 1803+), so no extra crates. Every
//! download is checked against the SHA-256 published by its source before anything lands in `bin\`.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::settings::Settings;
use crate::{applog, confgen, paths, tools};

const PHP_RELEASES: &str = "https://downloads.php.net/~windows/releases/";
const NODE_DIST: &str = "https://nodejs.org/dist/";
const COMPOSER_SITE: &str = "https://getcomposer.org";
const MAILPIT_LATEST_API: &str = "https://api.github.com/repos/axllent/mailpit/releases/latest";
/// Lists every Xdebug build with its SHA-256 (there is no machine-readable index).
const XDEBUG_DOWNLOAD_PAGE: &str = "https://xdebug.org/download";
const XDEBUG_FILES: &str = "https://xdebug.org/files/";

/// Stackyard's newest release; "latest" skips drafts and pre-releases.
const APP_LATEST_API: &str = "https://api.github.com/repos/thomasd333/stackyard/releases/latest";
/// Where an available Stackyard update is downloaded by hand.
pub const APP_RELEASES_PAGE: &str = "https://github.com/thomasd333/stackyard/releases/latest";
/// The automatic Stackyard update check (when switched on) runs at most this often.
const APP_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Node majors whose newest release is older than this (relative to the newest Node release) are
/// treated as end-of-life and not offered.
const NODE_ACTIVE_MONTHS: i64 = 8;

type DlResult<T> = Result<T, String>;

/// A version that can be downloaded.
#[derive(Clone)]
struct Release {
    tool: &'static str,
    /// Without a "v" prefix, e.g. "8.5.12-1", "22.23.2", "2.10.3".
    version: String,
    /// Install dir name for PHP and Node, e.g. "php-8.5.12-1-Win32-vs17-x64".
    target: String,
    url: String,
    /// Known up front for PHP; Node and Composer publish it in a separate file fetched at download time.
    sha256: Option<String>,
    /// Approximate size in bytes, for the progress display.
    size: Option<u64>,
    lts: bool,
}

/// A release that is not installed yet, as shown in the UI.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    version: String,
    lts: bool,
    /// The installed version of the same series this one updates, e.g. "8.5.11" for "8.5.12".
    updates: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct Updates {
    /// Tool id -> offers, newest first.
    offers: BTreeMap<&'static str, Vec<Offer>>,
    /// Sources that could not be checked.
    errors: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Downloading,
    Verifying,
    Extracting,
    Done,
    Error,
}

/// The current (or last finished) download.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    tool: &'static str,
    version: String,
    phase: Phase,
    bytes: u64,
    total: Option<u64>,
    /// Error text when `phase` is Error.
    message: Option<String>,
    /// Version dir to select after a successful install; None when the install is already active
    /// (flat Composer).
    label: Option<String>,
    /// PHP: label of the same-series install whose extra DLLs were copied, and their names.
    copied_from: Option<String>,
    copied: Vec<String>,
    /// PHP: another installed PHP has the SQL Server drivers but this one has none for its version.
    sqlsrv_missing: bool,
    /// Composer: file name the previous composer.phar was kept as.
    backup: Option<String>,
}

/// Result of the last Stackyard update check.
#[derive(Clone, Serialize)]
pub struct AppUpdate {
    /// Newest released version, e.g. "1.0.2".
    latest: String,
    /// True when it is newer than this Stackyard.
    newer: bool,
}

#[derive(Default)]
pub struct Downloads {
    /// Releases from the last check, with errors of the sources that failed.
    checked: Mutex<Option<(Vec<Release>, Vec<String>)>>,
    job: Mutex<Option<Job>>,
    app: Mutex<Option<AppUpdate>>,
    /// When Stackyard's own update was last checked (successfully or not).
    app_checked: Mutex<Option<Instant>>,
}

impl Downloads {
    /// Fetches the available versions from all sources (network, a few seconds).
    pub fn check(&self) -> Updates {
        let mut releases = Vec::new();
        let mut errors = Vec::new();
        for (name, fetch) in [
            ("PHP", fetch_php as fn() -> DlResult<Vec<Release>>),
            ("Node.js", fetch_node),
            ("Composer", fetch_composer),
            ("Mailpit", fetch_mailpit),
        ] {
            match fetch() {
                Ok(r) => releases.extend(r),
                Err(e) => {
                    applog::warn(format!("Update check: {name}: {e}"));
                    errors.push(format!("{name}: {e}"));
                }
            }
        }
        *self.checked.lock().unwrap() = Some((releases, errors));
        self.updates().unwrap_or_else(|| Updates { offers: BTreeMap::new(), errors: Vec::new() })
    }

    /// Offers from the last check against what is installed now; None before the first check.
    pub fn updates(&self) -> Option<Updates> {
        let (releases, errors) = self.checked.lock().unwrap().clone()?;
        let offers = ["php", "nodejs", "composer", "mailpit"]
            .into_iter()
            .map(|tool| {
                let mine: Vec<Release> = releases.iter().filter(|r| r.tool == tool).cloned().collect();
                (tool, offers(tool, &mine, &installed(tool)))
            })
            .collect();
        Some(Updates { offers, errors })
    }

    pub fn job(&self) -> Option<Job> {
        self.job.lock().unwrap().clone()
    }

    /// Forgets a finished job so the UI stops showing its result.
    pub fn dismiss(&self) {
        let mut job = self.job.lock().unwrap();
        if job.as_ref().is_some_and(|j| matches!(j.phase, Phase::Done | Phase::Error)) {
            *job = None;
        }
    }

    /// Starts downloading `version` of `tool` on a background thread.
    pub fn start(self: &Arc<Self>, tool: &str, version: &str) -> DlResult<()> {
        let release = self
            .checked
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|(r, _)| r.iter().find(|r| r.tool == tool && r.version == version).cloned())
            .ok_or_else(|| format!("{tool} {version} is not in the list of available versions. Check for updates again."))?;
        if tool != "composer" {
            let target = install_root(tool).join(&release.target);
            if target.exists() {
                return Err(format!("{} already exists.", target.display()));
            }
        }

        {
            let mut job = self.job.lock().unwrap();
            if job.as_ref().is_some_and(|j| !matches!(j.phase, Phase::Done | Phase::Error)) {
                return Err("Another download is in progress. Wait for it to finish.".into());
            }
            *job = Some(Job {
                tool: release.tool,
                version: release.version.clone(),
                phase: Phase::Downloading,
                bytes: 0,
                total: release.size,
                message: None,
                label: None,
                copied_from: None,
                copied: Vec::new(),
                sqlsrv_missing: false,
                backup: None,
            });
        }

        applog::info(format!("{}: downloading {} from {}", release.tool, release.version, release.url));
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let work = paths::tmp_dir().join("downloads");
            let result = install(&release, &work, &me);
            let _ = fs::remove_dir_all(&work);
            let mut job = me.job.lock().unwrap();
            let Some(job) = job.as_mut() else { return };
            match result {
                Ok(()) => {
                    applog::info(format!("{}: installed {}", release.tool, release.version));
                    job.phase = Phase::Done;
                }
                Err(e) => {
                    applog::error(format!("{}: download of {} failed: {e}", release.tool, release.version));
                    job.phase = Phase::Error;
                    job.message = Some(e);
                }
            }
        });
        Ok(())
    }

    /// Asks GitHub for the newest Stackyard release (network, a second or two).
    pub fn check_app(&self) -> DlResult<AppUpdate> {
        *self.app_checked.lock().unwrap() = Some(Instant::now());
        let latest = fetch_app_latest().inspect_err(|e| applog::warn(format!("Stackyard update check: {e}")))?;
        let update = AppUpdate { newer: is_newer(&latest, env!("CARGO_PKG_VERSION")), latest };
        if update.newer {
            applog::info(format!("Stackyard {} is available", update.latest));
        }
        *self.app.lock().unwrap() = Some(update.clone());
        Ok(update)
    }

    /// The last Stackyard update check's result (no network); None before the first one.
    pub fn app_update(&self) -> Option<AppUpdate> {
        self.app.lock().unwrap().clone()
    }

    /// Checks for a Stackyard update once a day while `startup.check_app_updates` is on (the first
    /// time right away), and passes each result to `notify`.
    pub fn watch_app_updates(self: &Arc<Self>, notify: impl Fn(&AppUpdate) + Send + 'static) {
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            loop {
                let due = me.app_checked.lock().unwrap().is_none_or(|t| t.elapsed() >= APP_CHECK_INTERVAL);
                if due
                    && Settings::load().startup.check_app_updates
                    && let Ok(update) = me.check_app()
                {
                    notify(&update);
                }
                std::thread::sleep(Duration::from_secs(10 * 60));
            }
        });
    }

    fn update_job(&self, f: impl FnOnce(&mut Job)) {
        if let Some(job) = self.job.lock().unwrap().as_mut() {
            f(job);
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Version sources

fn fetch_php() -> DlResult<Vec<Release>> {
    let json: Value = parse_json(&fetch_text(&format!("{PHP_RELEASES}releases.json"))?)?;
    let mut releases: Vec<Release> = json
        .as_object()
        .ok_or("unexpected releases.json format")?
        .values()
        .filter_map(|entry| {
            let version = entry["version"].as_str()?;
            // Apache's mod_php needs the Thread Safe build: keys like "ts-vs17-x64".
            let (_, build) =
                entry.as_object()?.iter().find(|(k, _)| k.starts_with("ts-") && k.ends_with("-x64"))?;
            let zip = &build["zip"];
            let path = zip["path"].as_str()?;
            Some(Release {
                tool: "php",
                version: version.to_string(),
                target: path.strip_suffix(".zip")?.to_string(),
                url: format!("{PHP_RELEASES}{path}"),
                sha256: zip["sha256"].as_str().map(str::to_lowercase),
                size: zip["size"].as_str().and_then(parse_size),
                lts: false,
            })
        })
        .collect();
    releases.sort_by(|a, b| tools::natural_cmp(&b.version, &a.version));
    Ok(releases)
}

fn fetch_node() -> DlResult<Vec<Release>> {
    let json: Value = parse_json(&fetch_text(&format!("{NODE_DIST}index.json"))?)?;
    let list = json.as_array().ok_or("unexpected index.json format")?;
    let newest = list.first().and_then(|r| r["date"].as_str()).and_then(month_index).unwrap_or(0);

    // The list is newest first; keep the newest Windows zip of each major that still gets releases.
    let mut releases: Vec<Release> = Vec::new();
    for r in list {
        let (Some(tag), Some(date)) = (r["version"].as_str(), r["date"].as_str()) else { continue };
        let has_zip = r["files"].as_array().is_some_and(|f| f.iter().any(|f| f == "win-x64-zip"));
        let version = tag.trim_start_matches('v');
        let major = series("nodejs", version);
        if !has_zip || releases.iter().any(|x| series("nodejs", &x.version) == major) {
            continue;
        }
        if month_index(date).is_none_or(|m| newest - m > NODE_ACTIVE_MONTHS) {
            continue;
        }
        let target = format!("node-{tag}-win-x64");
        releases.push(Release {
            tool: "nodejs",
            version: version.to_string(),
            url: format!("{NODE_DIST}{tag}/{target}.zip"),
            target,
            sha256: None,
            size: None,
            lts: r["lts"].is_string(),
        });
    }
    Ok(releases)
}

fn fetch_composer() -> DlResult<Vec<Release>> {
    let json: Value = parse_json(&fetch_text(&format!("{COMPOSER_SITE}/versions"))?)?;
    let mut releases: Vec<Release> = Vec::new();
    // Latest stable, plus the 2.2 LTS line (the last one supporting PHP < 7.2.5).
    for (channel, lts) in [("stable", false), ("2.2", true)] {
        let Some(entry) = json[channel].as_array().and_then(|a| a.first()) else { continue };
        let (Some(version), Some(path)) = (entry["version"].as_str(), entry["path"].as_str()) else { continue };
        if releases.iter().any(|r| r.version == version) {
            continue;
        }
        releases.push(Release {
            tool: "composer",
            version: version.to_string(),
            target: "composer.phar".into(),
            url: format!("{COMPOSER_SITE}{path}"),
            sha256: None,
            size: None,
            lts,
        });
    }
    Ok(releases)
}

/// Mailpit's newest release; GitHub publishes each asset's SHA-256 as its `digest`.
fn fetch_mailpit() -> DlResult<Vec<Release>> {
    let json = parse_json(&fetch_text(MAILPIT_LATEST_API)?)?;
    let tag = json["tag_name"].as_str().ok_or("unexpected GitHub response")?;
    let asset = json["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == "mailpit-windows-amd64.zip"))
        .ok_or("no Windows x64 build in the newest release")?;
    let version = tag.trim_start_matches('v').to_string();
    Ok(vec![Release {
        tool: "mailpit",
        target: format!("mailpit-{version}"),
        version,
        url: asset["browser_download_url"].as_str().ok_or("unexpected GitHub response")?.to_string(),
        sha256: asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_lowercase),
        size: asset["size"].as_u64(),
        lts: false,
    }])
}

/// The newest stable Xdebug for a PHP major.minor ("8.3"), thread safe or not, from xdebug.org's
/// download page: (file name, SHA-256). Each link reads
/// `title="SHA256:&nbsp;<hash>" href='/files/php_xdebug-3.5.3-8.3-ts-vs16-x86_64.dll'`.
fn find_xdebug(page: &str, php: &str, thread_safe: bool) -> Option<(String, String)> {
    let build = if thread_safe { "-ts-" } else { "-nts-" };
    let builds = page.split("SHA256:&nbsp;").skip(1).filter_map(|chunk| {
        let hash = chunk.get(..64).filter(|h| is_sha256(h))?.to_lowercase();
        let file = chunk.split("/files/").nth(1)?.split(['\'', '"']).next()?;
        let rest = file.strip_prefix("php_xdebug-")?.strip_suffix("-x86_64.dll")?;
        let (version, build_part) = rest.split_once(&format!("-{php}{build}"))?;
        // Stable only (no "3.6.0alpha1"), and the PHP version must match exactly ("8.3", not "8.30").
        let stable = version.chars().all(|c| c.is_ascii_digit() || c == '.');
        (stable && build_part.starts_with("vs")).then(|| (version.to_string(), file.to_string(), hash))
    });
    builds.max_by(|a, b| tools::natural_cmp(&a.0, &b.0)).map(|(_, file, hash)| (file, hash))
}

/// Downloads the newest stable Xdebug for this PHP into its `ext\` (a single DLL of a few hundred KB,
/// checked against the SHA-256 on xdebug.org). Returns the file name.
pub fn install_xdebug(php_dir: &Path) -> DlResult<String> {
    let label = php_dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let php = version_in(&label).map(|v| series("php", &v)).ok_or_else(|| format!("Cannot tell the PHP version of {label}."))?;
    let thread_safe = php_dir.join("php8ts.dll").is_file() || php_dir.join("php7ts.dll").is_file();
    let page = fetch_text(XDEBUG_DOWNLOAD_PAGE)?;
    let (file, expected) =
        find_xdebug(&page, &php, thread_safe).ok_or_else(|| format!("xdebug.org has no stable Xdebug for PHP {php} yet."))?;

    let work = paths::tmp_dir().join("downloads-xdebug");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("Cannot create {}: {e}", work.display()))?;
    let result = (|| {
        let tmp = work.join(&file);
        applog::info(format!("php: downloading {file} for {label}"));
        download(&format!("{XDEBUG_FILES}{file}"), &tmp, |_| {})?;
        let actual = sha256(&tmp)?;
        if actual != expected {
            return Err(format!(
                "The downloaded file is damaged or was changed (SHA-256 mismatch). Nothing was installed. Try again.\nExpected {expected}\nGot      {actual}"
            ));
        }
        // Copied, not moved: bin\ may be on another drive (a junction) than tmp\.
        let target = php_dir.join("ext").join(&file);
        fs::copy(&tmp, &target).map(|_| ()).map_err(|e| format!("Cannot copy {file} to {}: {e}", target.display()))
    })();
    let _ = fs::remove_dir_all(&work);
    result.map(|()| file)
}

/// Version of Stackyard's newest GitHub release, from its tag ("v1.0.2" -> "1.0.2").
fn fetch_app_latest() -> DlResult<String> {
    let json = parse_json(&fetch_text(APP_LATEST_API)?)?;
    let tag = json["tag_name"].as_str().ok_or("unexpected GitHub response")?;
    Ok(tag.trim_start_matches('v').to_string())
}

/// True when release `latest` is newer than `current` ("1.0.10" > "1.0.9").
fn is_newer(latest: &str, current: &str) -> bool {
    tools::natural_cmp(latest, current).is_gt()
}

/// SHA-256 published next to the download.
fn expected_sha256(r: &Release) -> DlResult<String> {
    if let Some(sha) = &r.sha256 {
        return Ok(sha.clone());
    }
    let found = match r.tool {
        "nodejs" => {
            let sums = fetch_text(&format!("{NODE_DIST}v{}/SHASUMS256.txt", r.version))?;
            sha_for(&sums, &format!("{}.zip", r.target))
        }
        _ => fetch_text(&format!("{}.sha256sum", r.url))?.split_whitespace().next().map(str::to_lowercase),
    };
    found.filter(|s| is_sha256(s)).ok_or_else(|| format!("No SHA-256 checksum published for {}", r.url))
}

/// Finds the hash of `file` in a "<hash>  <file>" list.
fn sha_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, name) = l.split_once(char::is_whitespace)?;
        (name.trim() == file).then(|| hash.to_lowercase())
    })
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// "30.5MB" -> bytes.
fn parse_size(s: &str) -> Option<u64> {
    let (num, mult) = match s.trim() {
        s if s.ends_with("MB") => (&s[..s.len() - 2], 1024.0 * 1024.0),
        s if s.ends_with("KB") => (&s[..s.len() - 2], 1024.0),
        s => (s, 1.0),
    };
    num.trim().parse::<f64>().ok().map(|n| (n * mult) as u64)
}

/// "2026-10-21" -> months since year 0.
fn month_index(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    Some(year * 12 + month)
}

fn parse_json(text: &str) -> DlResult<Value> {
    serde_json::from_str(text).map_err(|e| format!("unexpected response ({e})"))
}

// ---------------------------------------------------------------------------------------------------
// Installed versions and offers

/// First version number in `s`, keeping a PHP rebuild suffix:
/// "php-8.5.12-1-Win32-vs17-x64" -> "8.5.12-1", "node-v22.23.2-win-x64" -> "22.23.2", "v22" -> None.
fn version_in(s: &str) -> Option<String> {
    let mut rest = s;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        let tail = &rest[start..];
        let end = tail.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(tail.len());
        let version = tail[..end].trim_end_matches('.');
        if version.contains('.') {
            let after = &tail[version.len()..];
            let build = after.strip_prefix('-').map(|b| &b[..b.find(|c: char| !c.is_ascii_digit()).unwrap_or(b.len())]);
            return Some(match build {
                Some(b) if !b.is_empty() => format!("{version}-{b}"),
                _ => version.to_string(),
            });
        }
        rest = &tail[end.max(1)..];
    }
    None
}

/// Versions in the same series are patch updates of each other: PHP "8.5", Node "22", Composer "2.2"
/// (the LTS line) or "2".
fn series(tool: &str, version: &str) -> String {
    let parts: Vec<&str> = version.split(['.', '-']).collect();
    let take = match tool {
        "php" => 2,
        "composer" if parts.get(1) == Some(&"2") => 2,
        _ => 1,
    };
    parts.iter().take(take).copied().collect::<Vec<_>>().join(".")
}

/// (label, version) of each install of `tool`.
fn installed(tool: &str) -> Vec<(String, String)> {
    let Some(def) = tools::find(tool) else { return Vec::new() };
    def.installs(&paths::bin_dir())
        .into_iter()
        .filter_map(|i| {
            let version = match tool {
                "composer" => phar_version(&i.dir.join("composer.phar")),
                // "node-v22" has no full version in its name; ask node itself.
                _ => version_in(&i.label).filter(|v| v.matches('.').count() >= 2).or_else(|| {
                    def.read_version(&i.dir).and_then(|t| version_in(&t))
                }),
            }?;
            Some((i.label, version))
        })
        .collect()
}

fn offers(tool: &str, releases: &[Release], installed: &[(String, String)]) -> Vec<Offer> {
    releases
        .iter()
        .filter(|r| !installed.iter().any(|(label, v)| *label == r.target || *v == r.version))
        // Composer is replaced in place, so an older version is a downgrade, not something to offer.
        .filter(|r| {
            tool != "composer"
                || !installed.iter().any(|(_, v)| tools::natural_cmp(v, &r.version).is_ge())
        })
        .map(|r| {
            let s = series(tool, &r.version);
            let updates = installed
                .iter()
                .filter(|(_, v)| series(tool, v) == s && tools::natural_cmp(v, &r.version).is_lt())
                .max_by(|a, b| tools::natural_cmp(&a.1, &b.1))
                .map(|(_, v)| v.clone());
            Offer { version: r.version.clone(), lts: r.lts, updates }
        })
        .collect()
}

/// Version compiled into composer.phar (`const VERSION = '2.10.2'`).
fn phar_version(phar: &Path) -> Option<String> {
    let bytes = fs::read(phar).ok()?;
    const NEEDLE: &[u8] = b"const VERSION = '";
    bytes.windows(NEEDLE.len()).enumerate().filter(|(_, w)| *w == NEEDLE).find_map(|(i, _)| {
        let start = i + NEEDLE.len();
        let len = bytes[start..].iter().take(32).position(|&b| b == b'\'')?;
        let v = std::str::from_utf8(&bytes[start..start + len]).ok()?;
        v.starts_with(|c: char| c.is_ascii_digit()).then(|| v.to_string())
    })
}

fn install_root(tool: &str) -> PathBuf {
    paths::bin_dir().join(tool)
}

// ---------------------------------------------------------------------------------------------------
// Download and install

fn install(r: &Release, work: &Path, dl: &Downloads) -> DlResult<()> {
    let _ = fs::remove_dir_all(work);
    fs::create_dir_all(work).map_err(|e| format!("Cannot create {}: {e}", work.display()))?;

    let expected = expected_sha256(r)?;
    let file = work.join(r.url.rsplit('/').next().unwrap_or("download"));
    download(&r.url, &file, |bytes| dl.update_job(|j| j.bytes = bytes))?;

    dl.update_job(|j| j.phase = Phase::Verifying);
    let actual = sha256(&file)?;
    if actual != expected {
        return Err(format!(
            "The downloaded file is damaged or was changed (SHA-256 mismatch). Nothing was installed. Try again.\nExpected {expected}\nGot      {actual}"
        ));
    }

    if r.tool == "composer" {
        return install_composer(r, &file, dl);
    }

    dl.update_job(|j| j.phase = Phase::Extracting);
    let staging = work.join("extracted");
    extract(&file, &staging)?;
    let content = single_subdir(&staging).unwrap_or(staging);
    let target = install_root(r.tool).join(&r.target);
    if target.exists() {
        return Err(format!("{} already exists.", target.display()));
    }

    if r.tool == "php" {
        let s = series("php", &r.version);
        let settings = Settings::load();
        let selected = settings.selected_version("php");
        let mut same: Vec<(String, String)> = installed("php").into_iter().filter(|(_, v)| series("php", v) == s).collect();
        // Prefer the selected install as the source, else the newest of the series.
        same.sort_by_key(|(label, _)| Some(label.as_str()) != selected);
        if let Some((label, _)) = same.first() {
            let copied = carry_over(&install_root("php").join(label), &content)
                .map_err(|e| format!("Cannot copy extensions from {label}: {e}"))?;
            if !copied.is_empty() {
                applog::info(format!("php: copied from {label}: {}", copied.join(", ")));
            }
            let label = label.clone();
            dl.update_job(|j| {
                j.copied_from = Some(label);
                j.copied = copied;
            });
        }
    }

    fs::create_dir_all(install_root(r.tool)).map_err(|e| e.to_string())?;
    rename_retry(&content, &target)?;

    let label = r.target.clone();
    let sqlsrv_missing = r.tool == "php" && confgen::sqlsrv_extensions(&target).is_empty() && {
        let php = install_root("php");
        installed("php").iter().any(|(l, _)| *l != label && !confgen::sqlsrv_extensions(&php.join(l)).is_empty())
    };
    dl.update_job(|j| {
        j.label = Some(label);
        j.sqlsrv_missing = sqlsrv_missing;
    });
    Ok(())
}

/// Copies DLLs the old install has and the new one lacks: third-party extensions in `ext\` (e.g. the
/// SQL Server drivers) and their dependencies in the PHP root. Only meant for the same major.minor,
/// where extension binaries are compatible. Returns the copied file names.
fn carry_over(old: &Path, new: &Path) -> std::io::Result<Vec<String>> {
    let mut copied = Vec::new();
    for sub in ["ext", ""] {
        let (from, to) = (old.join(sub), new.join(sub));
        let Ok(entries) = fs::read_dir(&from) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let is_dll = name.to_string_lossy().to_lowercase().ends_with(".dll");
            if is_dll && entry.path().is_file() && !to.join(&name).exists() {
                fs::create_dir_all(&to)?;
                fs::copy(entry.path(), to.join(&name))?;
                copied.push(name.to_string_lossy().into_owned());
            }
        }
    }
    Ok(copied)
}

/// Flat install (or none yet): replace `bin\composer\composer.phar`, keeping the old file.
/// Versioned installs: a new `bin\composer\composer-<version>\`.
fn install_composer(r: &Release, phar: &Path, dl: &Downloads) -> DlResult<()> {
    let root = install_root("composer");
    let flat = tools::find("composer")
        .map(|t| t.installs(&paths::bin_dir()))
        .is_none_or(|i| i.first().is_none_or(|i| i.label == "default"));
    let dir = if flat { root.clone() } else { root.join(format!("composer-{}", r.version)) };
    if !flat && dir.exists() {
        return Err(format!("{} already exists.", dir.display()));
    }
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;

    let target = dir.join("composer.phar");
    let mut backup = None;
    if target.exists() {
        let old = phar_version(&target).unwrap_or_else(|| "old".into());
        let name = format!("composer-{old}.phar");
        let _ = fs::remove_file(dir.join(&name));
        rename_retry(&target, &dir.join(&name))?;
        backup = Some(name);
    }
    if let Err(e) = rename_retry(phar, &target) {
        if let Some(name) = &backup {
            let _ = fs::rename(dir.join(name), &target);
        }
        return Err(e);
    }
    write_composer_wrappers(&dir).map_err(|e| format!("Cannot write the composer command: {e}"))?;

    let label = (!flat).then(|| format!("composer-{}", r.version));
    dl.update_job(|j| {
        j.label = label;
        j.backup = backup;
    });
    Ok(())
}

/// `composer` for cmd/PowerShell and Git Bash, unless they already exist.
fn write_composer_wrappers(dir: &Path) -> std::io::Result<()> {
    let bat = dir.join("composer.bat");
    if !bat.exists() {
        fs::write(bat, "@ECHO OFF\r\nphp \"%~dp0composer.phar\" %*\r\n")?;
    }
    let sh = dir.join("composer");
    if !sh.exists() {
        fs::write(sh, "#!/bin/sh\ndir=$(cd \"$(dirname \"$0\")\" && (pwd -W 2>/dev/null || pwd))\nphp \"$dir/composer.phar\" \"$@\"\n")?;
    }
    Ok(())
}

fn system_exe(name: &str) -> PathBuf {
    let windir = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    windir.join("System32").join(name)
}

fn hidden(exe: &str) -> Command {
    let mut cmd = Command::new(system_exe(exe));
    cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
    cmd
}

fn curl_args(cmd: &mut Command) -> &mut Command {
    // Fail on HTTP errors, follow redirects, give up on a stalled connection.
    cmd.args(["-fsSL", "--connect-timeout", "20", "--speed-limit", "1024", "--speed-time", "60"])
}

fn fetch_text(url: &str) -> DlResult<String> {
    let out = curl_args(&mut hidden("curl.exe"))
        .args(["--max-time", "60", url])
        .output()
        .map_err(|e| format!("Cannot run curl.exe: {e}"))?;
    if !out.status.success() {
        return Err(curl_error(url, &out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn curl_error(url: &str, stderr: &[u8]) -> String {
    let msg = String::from_utf8_lossy(stderr).trim().to_string();
    format!("Cannot download {url}: {}. Check the internet connection.", if msg.is_empty() { "unknown error" } else { &msg })
}

/// Downloads to `dest`, reporting the bytes written so far.
fn download(url: &str, dest: &Path, progress: impl Fn(u64)) -> DlResult<()> {
    let mut child = curl_args(&mut hidden("curl.exe"))
        .arg("-o")
        .arg(dest)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot run curl.exe: {e}"))?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        progress(fs::metadata(dest).map(|m| m.len()).unwrap_or(0));
        std::thread::sleep(Duration::from_millis(250));
    };
    progress(fs::metadata(dest).map(|m| m.len()).unwrap_or(0));
    if !status.success() {
        let mut stderr = Vec::new();
        if let Some(mut s) = child.stderr.take() {
            let _ = s.read_to_end(&mut stderr);
        }
        return Err(curl_error(url, &stderr));
    }
    Ok(())
}

fn sha256(file: &Path) -> DlResult<String> {
    let out = hidden("certutil.exe")
        .arg("-hashfile")
        .arg(file)
        .arg("SHA256")
        .output()
        .map_err(|e| format!("Cannot run certutil.exe: {e}"))?;
    // Line 2 is the hash; older Windows separates the bytes with spaces.
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.replace(' ', "").to_lowercase())
        .find(|l| is_sha256(l))
        .ok_or_else(|| format!("Cannot compute the checksum of {}", file.display()))
}

fn extract(zip: &Path, into: &Path) -> DlResult<()> {
    fs::create_dir_all(into).map_err(|e| e.to_string())?;
    let out = hidden("tar.exe")
        .arg("-xf")
        .arg(zip)
        .arg("-C")
        .arg(into)
        .output()
        .map_err(|e| format!("Cannot run tar.exe: {e}"))?;
    if !out.status.success() {
        return Err(format!("Cannot unpack {}: {}", zip.display(), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// The only entry of `dir` when it is a directory (Node's zip has a top-level folder, PHP's doesn't).
fn single_subdir(dir: &Path) -> Option<PathBuf> {
    let mut entries = fs::read_dir(dir).ok()?.flatten();
    let first = entries.next()?;
    (entries.next().is_none() && first.path().is_dir()).then(|| first.path())
}

/// Antivirus scanners briefly lock freshly written files, so a rename can fail for a moment.
fn rename_retry(from: &Path, to: &Path) -> DlResult<()> {
    let mut last = None;
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(500));
        }
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) => last = Some(e),
        }
    }
    Err(format!("Cannot move {} to {}: {}", from.display(), to.display(), last.map(|e| e.to_string()).unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tool: &'static str, version: &str, target: &str) -> Release {
        Release {
            tool,
            version: version.into(),
            target: target.into(),
            url: String::new(),
            sha256: None,
            size: None,
            lts: false,
        }
    }

    #[test]
    fn versions_from_names() {
        assert_eq!(version_in("php-8.5.12-1-Win32-vs17-x64").as_deref(), Some("8.5.12-1"));
        assert_eq!(version_in("php-8.3.33-Win32-vs16-x64").as_deref(), Some("8.3.33"));
        assert_eq!(version_in("node-v22.23.2-win-x64").as_deref(), Some("22.23.2"));
        assert_eq!(version_in("v22.23.2").as_deref(), Some("22.23.2"));
        assert_eq!(version_in("PHP 8.3.33 (cli)").as_deref(), Some("8.3.33"));
        assert_eq!(version_in("node-v22"), None);
    }

    #[test]
    fn series_per_tool() {
        assert_eq!(series("php", "8.5.12-1"), "8.5");
        assert_eq!(series("nodejs", "22.23.2"), "22");
        assert_eq!(series("composer", "2.2.30"), "2.2");
        assert_eq!(series("composer", "2.10.3"), "2");
    }

    #[test]
    fn offers_mark_updates_and_skip_installed() {
        let releases = [
            rel("php", "8.5.12-1", "php-8.5.12-1-Win32-vs17-x64"),
            rel("php", "8.4.27-1", "php-8.4.27-1-Win32-vs17-x64"),
            rel("php", "8.3.36-1", "php-8.3.36-1-Win32-vs16-x64"),
        ];
        let installed = [
            ("php-8.5.12-1-Win32-vs17-x64".to_string(), "8.5.12-1".to_string()),
            ("php-8.3.33-Win32-vs16-x64".to_string(), "8.3.33".to_string()),
        ];
        let o = offers("php", &releases, &installed);
        assert_eq!(o.len(), 2);
        assert_eq!((o[0].version.as_str(), o[0].updates.as_deref()), ("8.4.27-1", None));
        assert_eq!((o[1].version.as_str(), o[1].updates.as_deref()), ("8.3.36-1", Some("8.3.33")));
    }

    #[test]
    fn composer_offers_no_downgrade() {
        let releases = [rel("composer", "2.10.3", "composer.phar"), rel("composer", "2.2.30", "composer.phar")];
        let o = offers("composer", &releases, &[("default".into(), "2.10.2".into())]);
        assert_eq!(o.len(), 1);
        assert_eq!(o[0].updates.as_deref(), Some("2.10.2"));
        assert_eq!(offers("composer", &releases, &[]).len(), 2);
    }

    #[test]
    fn app_versions_compare_numerically() {
        assert!(is_newer("1.0.2", "1.0.1"));
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(is_newer("1.1.0", "1.0.99"));
        assert!(!is_newer("1.0.1", "1.0.1"));
        assert!(!is_newer("1.0.0", "1.0.1"));
    }

    /// Asks GitHub for the newest Stackyard release: `cargo test app_check_real -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn app_check_real() {
        let update = Downloads::default().check_app().unwrap();
        println!("latest {} (newer than {}: {})", update.latest, env!("CARGO_PKG_VERSION"), update.newer);
        assert!(update.latest.starts_with(|c: char| c.is_ascii_digit()));
    }

    #[test]
    fn xdebug_build_for_php() {
        let link = |hash: char, file: &str| format!("<li><a title=\"SHA256:&nbsp;{}\" href='/files/{file}'>x</a></li>\n", hash.to_string().repeat(64));
        let page = [
            link('a', "php_xdebug-3.6.0alpha1-8.3-ts-vs16-x86_64.dll"),
            link('b', "php_xdebug-3.5.3-8.3-nts-vs16-x86_64.dll"),
            link('c', "php_xdebug-3.5.3-8.3-ts-vs16-x86_64.dll"),
            link('d', "php_xdebug-3.5.3-8.5-ts-vs17-x86_64.dll"),
            link('e', "php_xdebug-3.5.10-8.3-ts-vs16-x86_64.dll"),
            link('f', "php_xdebug-3.5.3-8.3-ts-vs16-x86.dll"),
        ]
        .concat();
        let found = find_xdebug(&page, "8.3", true).unwrap();
        assert_eq!(found, ("php_xdebug-3.5.10-8.3-ts-vs16-x86_64.dll".to_string(), "e".repeat(64)));
        assert_eq!(find_xdebug(&page, "8.3", false).unwrap().1, "b".repeat(64));
        assert_eq!(find_xdebug(&page, "8.5", true).unwrap().0, "php_xdebug-3.5.3-8.5-ts-vs17-x86_64.dll");
        assert!(find_xdebug(&page, "8.4", true).is_none());
    }

    /// Downloads Xdebug into a fake PHP 8.5 TS folder (network, ~1 MB):
    /// `cargo test xdebug_real -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn xdebug_real() {
        let php = std::env::temp_dir().join(format!("stackyard-xdebug-{}/php-8.5.12-1-Win32-vs17-x64", std::process::id()));
        fs::create_dir_all(php.join("ext")).unwrap();
        fs::write(php.join("php8ts.dll"), "").unwrap();
        let file = install_xdebug(&php).unwrap();
        println!("{file} {} bytes", fs::metadata(php.join("ext").join(&file)).unwrap().len());
        assert!(confgen::xdebug_dll(&php).is_some());
        fs::remove_dir_all(php.parent().unwrap()).unwrap();
    }

    #[test]
    fn checksums_and_sizes() {
        let sums = "aaa  node-v22.1.0-win-x86.zip\nBBB  node-v22.1.0-win-x64.zip\n";
        assert_eq!(sha_for(sums, "node-v22.1.0-win-x64.zip").as_deref(), Some("bbb"));
        assert_eq!(parse_size("25.02MB"), Some(26235371));
        assert_eq!(month_index("2026-10-21"), Some(2026 * 12 + 10));
    }

    #[test]
    fn carry_over_copies_only_missing_dlls() {
        let base = std::env::temp_dir().join(format!("stackyard-carry-{}", std::process::id()));
        let (old, new) = (base.join("old"), base.join("new"));
        for d in [old.join("ext"), new.join("ext")] {
            fs::create_dir_all(d).unwrap();
        }
        for f in ["ext/php_curl.dll", "ext/php_sqlsrv_85_ts_x64.dll", "CORE_RL_magick.dll", "php.ini", "php8ts.dll"] {
            fs::write(old.join(f), "old").unwrap();
        }
        for f in ["ext/php_curl.dll", "php8ts.dll"] {
            fs::write(new.join(f), "new").unwrap();
        }
        let mut copied = carry_over(&old, &new).unwrap();
        copied.sort();
        assert_eq!(copied, ["CORE_RL_magick.dll", "php_sqlsrv_85_ts_x64.dll"]);
        assert_eq!(fs::read_to_string(new.join("ext/php_curl.dll")).unwrap(), "new");
        assert!(!new.join("php.ini").exists());
        fs::remove_dir_all(base).unwrap();
    }

    /// Real downloads into a temporary root (network, ~70 MB):
    /// `cargo test install_real -- --ignored --nocapture`. Seeds a fake PHP 8.3 with the real sqlsrv DLLs
    /// and a copy of the real composer.phar, then installs the newest PHP 8.3, Node 22 and Composer.
    #[test]
    #[ignore]
    fn install_real() {
        let real_bin = paths::bin_dir();
        let root = std::env::temp_dir().join(format!("stackyard-dl-{}", std::process::id()));
        let old_php = root.join("bin/php/php-8.3.0-Win32-vs16-x64");
        fs::create_dir_all(old_php.join("ext")).unwrap();
        fs::create_dir_all(root.join("bin/composer")).unwrap();
        fs::write(old_php.join("php.exe"), "").unwrap();
        let real_ext = real_bin.join("php/php-8.3.33-Win32-vs16-x64/ext");
        for dll in ["php_sqlsrv_83_ts_x64.dll", "php_pdo_sqlsrv_83_ts_x64.dll"] {
            fs::copy(real_ext.join(dll), old_php.join("ext").join(dll)).unwrap();
        }
        fs::copy(real_bin.join("composer/composer.phar"), root.join("bin/composer/composer.phar")).unwrap();
        // SAFETY: ignored test, run on its own.
        unsafe { std::env::set_var("STACKYARD_ROOT", &root) };

        let dl = Arc::new(Downloads::default());
        let updates = dl.check();
        assert!(updates.errors.is_empty(), "{:?}", updates.errors);
        let pick = |tool: &str, prefix: &str| {
            updates.offers[tool].iter().find(|o| o.version.starts_with(prefix)).unwrap().version.clone()
        };
        for (tool, version) in [("php", pick("php", "8.3.")), ("nodejs", pick("nodejs", "22.")), ("composer", pick("composer", "2."))] {
            dl.start(tool, &version).unwrap();
            let job = loop {
                let job = dl.job().unwrap();
                if matches!(job.phase, Phase::Done | Phase::Error) {
                    break job;
                }
                std::thread::sleep(Duration::from_millis(500));
            };
            println!(
                "{tool} {version}: {:?} {:?} label={:?} from={:?} copied={:?} sqlsrv_missing={} backup={:?} {} bytes",
                job.phase == Phase::Done, job.message, job.label, job.copied_from, job.copied, job.sqlsrv_missing, job.backup, job.bytes
            );
            assert!(job.phase == Phase::Done);
        }
        let php = install_root("php");
        let new_php = installed("php").into_iter().find(|(_, v)| v != "8.3.0").unwrap().0;
        assert!(php.join(&new_php).join("php.exe").is_file());
        assert!(php.join(&new_php).join("ext/php_sqlsrv_83_ts_x64.dll").is_file());
        assert_eq!(installed("composer")[0].1, pick("composer", "2."));
        assert!(installed("nodejs")[0].0.starts_with("node-v22."));
        assert!(dl.updates().unwrap().offers["composer"].is_empty());
        println!("{:?}", fs::read_dir(root.join("bin/composer")).unwrap().flatten().map(|e| e.file_name()).collect::<Vec<_>>());
        assert!(!paths::tmp_dir().join("downloads").exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// Lists what can be downloaded right now (network): `cargo test check_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn check_real() {
        for (name, fetch) in [
            ("php", fetch_php as fn() -> DlResult<Vec<Release>>),
            ("nodejs", fetch_node),
            ("composer", fetch_composer),
            ("mailpit", fetch_mailpit),
        ] {
            for r in fetch().unwrap() {
                println!("{name:<9} {:<10} lts={:<5} {} {:?} {:?}", r.version, r.lts, r.url, r.size, r.sha256);
            }
        }
    }
}
