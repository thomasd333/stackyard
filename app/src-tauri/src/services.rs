//! Starting, stopping and supervising Apache and MySQL.
//!
//! Start/stop work runs on background threads; the UI polls `snapshot()` for status.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::applog;
use crate::confgen;
use crate::paths;
use crate::ports;
use crate::process::{self, Proc, Stop};
use crate::settings::Settings;
use crate::tools::{self, Install};

pub const SERVICES: &[(&str, &str)] = &[("apache", "Apache"), ("mysql", "MySQL")];

/// Windows STATUS_DLL_NOT_FOUND, typically a missing Visual C++ Redistributable.
const DLL_NOT_FOUND: i32 = 0xC000_0135_u32 as i32;

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

#[derive(Default)]
struct Slot {
    status: Status,
    message: Option<String>,
    proc: Option<Proc>,
    port: Option<u16>,
}

#[derive(Serialize, PartialEq)]
pub struct ServiceInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub status: Status,
    pub message: Option<String>,
    pub port: u16,
    pub pid: Option<u32>,
    pub version: Option<String>,
}

#[derive(Default)]
pub struct Services {
    slots: Mutex<HashMap<&'static str, Slot>>,
}

type StartResult = Result<(Proc, u16), String>;

impl Services {
    pub fn snapshot(&self, settings: &Settings) -> Vec<ServiceInfo> {
        let mut slots = self.slots.lock().unwrap();
        SERVICES
            .iter()
            .map(|&(id, name)| {
                let slot = slots.entry(id).or_default();
                reap(id, slot);
                ServiceInfo {
                    id,
                    name,
                    status: slot.status,
                    message: slot.message.clone(),
                    port: slot.port.filter(|_| slot.proc.is_some()).unwrap_or(configured_port(id, settings)),
                    pid: slot.proc.as_ref().map(Proc::pid),
                    version: selected(id, settings).map(|i| i.label),
                }
            })
            .collect()
    }

    pub fn start(self: &Arc<Self>, id: &'static str) {
        {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            reap(id, slot);
            if matches!(slot.status, Status::Starting | Status::Running | Status::Stopping) {
                return;
            }
            *slot = Slot { status: Status::Starting, ..Slot::default() };
        }

        applog::info(format!("{id}: starting"));
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let result = me.launch(id);
            let mut slots = me.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            *slot = match result {
                Ok((proc, port)) => {
                    applog::info(format!("{id}: running (PID {}, port {port})", proc.pid()));
                    Slot { status: Status::Running, message: None, proc: Some(proc), port: Some(port) }
                }
                Err(msg) => {
                    applog::error(format!("{id}: start failed: {msg}"));
                    Slot { status: Status::Error, message: Some(msg), ..Slot::default() }
                }
            };
        });
    }

    pub fn stop(self: &Arc<Self>, id: &'static str) {
        let Some(proc) = self.take_for_stop(id) else {
            // Stopping a failed service just clears its error.
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            if slot.status == Status::Error {
                *slot = Slot::default();
            }
            return;
        };

        let me = Arc::clone(self);
        std::thread::spawn(move || {
            shutdown(id, proc);
            me.reset(id);
        });
    }

    /// Restarts a running service (e.g. after a version change); does nothing if it isn't running.
    pub fn restart(self: &Arc<Self>, id: &'static str) {
        let Some(proc) = self.take_for_stop(id) else { return };
        applog::info(format!("{id}: restarting"));
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            shutdown(id, proc);
            me.reset(id);
            me.start(id);
        });
    }

    /// Takes the running process out of its slot, marking the service as stopping.
    fn take_for_stop(&self, id: &'static str) -> Option<Proc> {
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.entry(id).or_default();
        let proc = slot.proc.take()?;
        slot.status = Status::Stopping;
        applog::info(format!("{id}: stopping (PID {})", proc.pid()));
        Some(proc)
    }

    fn reset(&self, id: &'static str) {
        *self.slots.lock().unwrap().entry(id).or_default() = Slot::default();
    }

    /// Gracefully stops everything and waits for it; used when the app exits.
    pub fn stop_all_blocking(&self) {
        let procs: Vec<(&'static str, Proc)> = self
            .slots
            .lock()
            .unwrap()
            .iter_mut()
            .filter_map(|(id, slot)| slot.proc.take().map(|p| (*id, p)))
            .collect();
        std::thread::scope(|s| {
            for (id, proc) in procs {
                s.spawn(move || shutdown(id, proc));
            }
        });
    }

    fn set_message(&self, id: &'static str, message: &str) {
        self.slots.lock().unwrap().entry(id).or_default().message = Some(message.to_string());
    }

    fn launch(&self, id: &'static str) -> StartResult {
        if !paths::root_is_supported() {
            return Err(paths::UNSUPPORTED_ROOT_MESSAGE.into());
        }
        confgen::ensure_layout()?;
        let settings = Settings::load();
        let port = configured_port(id, &settings);
        if !ports::is_free(port) {
            return Err(format!(
                "Port {port} is already in use by {}.\nStop that program or change the port in config\\settings.json.",
                ports::describe_owner(port)
            ));
        }
        match id {
            "apache" => self.launch_apache(&settings, port),
            "mysql" => self.launch_mysql(&settings, port),
            _ => Err(format!("Unknown service: {id}")),
        }
    }

    fn launch_apache(&self, settings: &Settings, port: u16) -> StartResult {
        let apache = selected("apache", settings).ok_or("Apache is not installed (bin\\apache).")?;
        let php = selected("php", settings);
        let conf = confgen::write_apache(&apache.dir, php.as_ref().map(|p| p.dir.as_path()), port, &settings.sites)?;
        let httpd = apache.dir.join("bin/httpd.exe");
        let extra_path: Vec<PathBuf> = php.iter().map(|p| p.dir.clone()).chain([apache.dir.join("bin")]).collect();
        applog::info(format!(
            "apache: {} with {} on port {port}",
            apache.label,
            php.as_ref().map_or("no PHP", |p| p.label.as_str())
        ));
        applog::rotate(&["apache_error.log", "apache_access.log", "php_error.log"]);

        // Config test first: gives a precise message instead of a silent exit.
        let test = Command::new(&httpd)
            .args(["-t", "-f"])
            .arg(&conf)
            .env("PATH", child_path(&extra_path))
            .creation_flags_hidden()
            .output()
            .map_err(|e| format!("Cannot run {}: {e}", httpd.display()))?;
        if !test.status.success() {
            let out = String::from_utf8_lossy(&test.stderr).trim().to_string();
            return Err(explain_exit(test.status.code(), &format!("Apache config test failed: {out}")));
        }

        let _ = std::fs::remove_file(paths::tmp_dir().join("httpd.pid"));
        let mut cmd = Command::new(&httpd);
        cmd.arg("-f").arg(&conf).env("PATH", child_path(&extra_path));
        let log = paths::logs_dir().join("apache_console.log");
        let mut proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot start Apache: {e}"))?;
        proc.set_stop(Stop::Event(format!("ap{}_shutdown", proc.pid())));
        wait_ready(proc, port, Duration::from_secs(15), &[&log, &paths::logs_dir().join("apache_error.log")])
            .map(|p| (p, port))
    }

    fn launch_mysql(&self, settings: &Settings, port: u16) -> StartResult {
        let mysql = selected("mysql", settings).ok_or("MySQL is not installed (bin\\mysql).")?;
        let datadir = confgen::mysql_datadir(&mysql.label);
        let ini = confgen::write_mysql(&mysql.dir, &datadir, port)?;
        let mysqld = mysql.dir.join("bin/mysqld.exe");
        let logs = paths::logs_dir();
        let defaults = format!("--defaults-file={}", ini.display());
        applog::info(format!("mysql: {} on port {port}, datadir {}", mysql.label, datadir.display()));
        applog::rotate(&["mysql_error.log"]);

        if !datadir.join("mysql.ibd").exists() {
            applog::info("mysql: initializing a new data directory");
            self.set_message("mysql", "Initializing data directory (first start)…");
            let log = logs.join("mysql_init.log");
            let mut cmd = Command::new(&mysqld);
            cmd.args([defaults.as_str(), "--initialize-insecure", "--console"]);
            let mut proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot run mysqld: {e}"))?;
            if !proc.wait_for_exit(Duration::from_secs(180)) {
                proc.kill();
                return Err("MySQL initialization timed out.".into());
            }
            let code = proc.exited().flatten();
            if code != Some(0) {
                return Err(explain_exit(code, &format!("MySQL initialization failed.\n{}", tail(&[&log], 6))));
            }
            self.set_message("mysql", "Starting…");
        }

        let mut cmd = Command::new(&mysqld);
        cmd.arg(&defaults);
        let log = logs.join("mysql_console.log");
        let mut proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot start MySQL: {e}"))?;
        // mysqld has no shutdown event on Windows, so stop it with mysqladmin: as our shutdown-only account
        // (works after root gets a password), then as root without a password (the default).
        let admin = mysql.dir.join("bin/mysqladmin.exe");
        let shutdown_as = |first: String| {
            let mut c = Command::new(&admin);
            c.arg(first).args(["--host=127.0.0.1", "--connect-timeout=5", &format!("--port={port}"), "shutdown"]);
            c
        };
        let mut stops = vec![shutdown_as("--user=root".into())];
        if let Ok(cnf) = shutdown_cnf() {
            stops.insert(0, shutdown_as(format!("--defaults-extra-file={}", cnf.display())));
        }
        proc.set_stop(Stop::Commands(stops));
        let proc = wait_ready(proc, port, Duration::from_secs(60), &[&log, &logs.join("mysql_error.log")])?;
        ensure_shutdown_account(&mysql.dir, port);
        Ok((proc, port))
    }
}

const SHUTDOWN_USER: &str = "stackyard_shutdown";

/// `config\mysql\shutdown.cnf`: client options for the shutdown-only account, with a random password made
/// on first use. It lives outside the datadir, so every MySQL version's datadir gets the same account.
fn shutdown_cnf() -> std::io::Result<PathBuf> {
    let path = paths::config_dir().join("mysql").join("shutdown.cnf");
    if !path.is_file() {
        use std::hash::{BuildHasher, RandomState};
        // RandomState is seeded from the OS, which is plenty for a local account that can only shut down.
        let password: String = (0..3).map(|i| format!("{:016x}", RandomState::new().hash_one(i))).collect();
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, format!("[client]\nuser={SHUTDOWN_USER}\npassword={password}\n"))?;
    }
    Ok(path)
}

fn hidden_status(cmd: &mut Command) -> bool {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    cmd.status().is_ok_and(|s| s.success())
}

/// Makes sure the shutdown-only account exists and matches shutdown.cnf. It can only be created while root
/// has no password; once it exists, root may get one and stopping stays clean.
fn ensure_shutdown_account(mysql_dir: &Path, port: u16) {
    let Ok(cnf) = shutdown_cnf() else { return };
    let client = mysql_dir.join("bin/mysql.exe");
    let port = format!("--port={port}");
    let base = ["--host=127.0.0.1", "--connect-timeout=5", port.as_str(), "--batch"];
    let mut check = Command::new(&client);
    check.arg(format!("--defaults-extra-file={}", cnf.display())).args(base).args(["-e", "SELECT 1"]);
    if hidden_status(&mut check) {
        return;
    }
    let text = std::fs::read_to_string(&cnf).unwrap_or_default();
    let Some(password) = text.lines().find_map(|l| l.strip_prefix("password=")) else { return };
    let sql: String = ["localhost", "127.0.0.1"]
        .iter()
        .map(|host| {
            let user = format!("'{SHUTDOWN_USER}'@'{host}'");
            format!(
                "CREATE USER IF NOT EXISTS {user} IDENTIFIED BY '{password}'; \
                 ALTER USER {user} IDENTIFIED BY '{password}'; GRANT SHUTDOWN ON *.* TO {user}; "
            )
        })
        .collect();
    let mut create = Command::new(&client);
    create.arg("--user=root").args(base).args(["-e", &sql]);
    if hidden_status(&mut create) {
        applog::info(format!("mysql: shutdown account {SHUTDOWN_USER} set up"));
    } else {
        applog::warn("mysql: cannot set up the shutdown account (root has a password?); stop falls back to root");
    }
}

fn configured_port(id: &str, settings: &Settings) -> u16 {
    match id {
        "mysql" => settings.ports.mysql,
        _ => settings.ports.apache,
    }
}

/// The install chosen in settings, or the newest one.
pub(crate) fn selected(tool_id: &str, settings: &Settings) -> Option<Install> {
    let tool = tools::find(tool_id)?;
    let mut installs = tool.installs(&paths::bin_dir());
    let wanted = settings.selected_version(tool_id);
    let idx = installs.iter().position(|i| Some(i.label.as_str()) == wanted).unwrap_or(0);
    (idx < installs.len()).then(|| installs.swap_remove(idx))
}

/// Marks a running service as failed if its process died on its own.
fn reap(id: &str, slot: &mut Slot) {
    let Some(code) = slot.proc.as_mut().and_then(Proc::exited) else { return };
    slot.proc = None;
    slot.status = Status::Error;
    let logs = paths::logs_dir();
    let log = logs.join(format!("{}_error.log", if id == "mysql" { "mysql" } else { "apache" }));
    let message = explain_exit(code, &format!("Process exited unexpectedly.\n{}", tail(&[&log], 4)));
    applog::error(format!("{id}: exited unexpectedly (code {code:?}): {message}"));
    slot.message = Some(message);
}

fn shutdown(id: &str, proc: Proc) {
    if proc.shutdown(Duration::from_secs(if id == "mysql" { 30 } else { 10 })) {
        applog::info(format!("{id}: stopped"));
    } else {
        applog::warn(format!("{id}: did not stop cleanly, process tree was killed"));
    }
}

/// Waits until the service accepts connections; on failure kills it and reports the log tail.
fn wait_ready(mut proc: Proc, port: u16, timeout: Duration, logs: &[&Path]) -> Result<Proc, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(code) = proc.exited() {
            return Err(explain_exit(code, &format!("Process exited during startup.\n{}", tail(logs, 6))));
        }
        if ports::is_listening(port) {
            return Ok(proc);
        }
        if Instant::now() > deadline {
            proc.kill();
            return Err(format!("Not listening on port {port} after {}s.\n{}", timeout.as_secs(), tail(logs, 6)));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn explain_exit(code: Option<i32>, message: &str) -> String {
    if code == Some(DLL_NOT_FOUND) {
        return "A required DLL is missing - install the latest Microsoft Visual C++ Redistributable (x64).".into();
    }
    message.trim().to_string()
}

/// Last `n` non-empty lines across the given log files.
fn tail(files: &[&Path], n: usize) -> String {
    let lines: Vec<String> = files
        .iter()
        .filter_map(|f| std::fs::read(f).ok())
        .flat_map(|bytes| String::from_utf8_lossy(&bytes).lines().map(str::to_string).collect::<Vec<_>>())
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// PATH for child processes: `extra` dirs first (so e.g. PHP finds its DLLs), then ours.
fn child_path(extra: &[PathBuf]) -> std::ffi::OsString {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let dirs = extra.iter().cloned().chain(std::env::split_paths(&current));
    std::env::join_paths(dirs).unwrap_or(current)
}

trait HiddenCommand {
    fn creation_flags_hidden(&mut self) -> &mut Self;
}

impl HiddenCommand for Command {
    fn creation_flags_hidden(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_while(services: &Services, status: Status, timeout: Duration) -> Vec<ServiceInfo> {
        let deadline = Instant::now() + timeout;
        loop {
            let snap = services.snapshot(&Settings::load());
            if snap.iter().all(|s| s.status != status) || Instant::now() > deadline {
                return snap;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Starts and stops the real services from `bin/`: `cargo test start_stop_real -- --ignored --nocapture`.
    /// Set `STACKYARD_TEST_SERVICES=apache` to test only Apache (e.g. while another MySQL uses the datadir).
    #[test]
    #[ignore]
    fn start_stop_real() {
        unsafe {
            use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetErrorMode};
            SetErrorMode(SEM_FAILCRITICALERRORS);
        }
        let wanted = std::env::var("STACKYARD_TEST_SERVICES").unwrap_or_else(|_| "apache,mysql".into());
        let ids: Vec<&'static str> =
            SERVICES.iter().map(|(id, _)| *id).filter(|id| wanted.split(',').any(|w| w == *id)).collect();
        let services = Arc::new(Services::default());
        for id in &ids {
            services.start(id);
        }
        for s in wait_while(&services, Status::Starting, Duration::from_secs(240)) {
            if !ids.contains(&s.id) {
                continue;
            }
            println!("{:<7} {:?} port={} pid={:?} msg={:?}", s.id, s.status as u8, s.port, s.pid, s.message);
            assert!(s.status == Status::Running, "{} did not start: {:?}", s.id, s.message);
        }

        // Fetch the start page through Apache + PHP.
        use std::io::{Read, Write};
        let settings = Settings::load();
        let get = |host: &str| {
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", settings.ports.apache)).unwrap();
            conn.write_all(format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n").as_bytes()).unwrap();
            let mut page = String::new();
            conn.read_to_string(&mut page).unwrap();
            page
        };
        if ids.contains(&"apache") {
            let page = get("localhost");
            println!("HTTP: {}", page.lines().next().unwrap_or_default());
            println!("Page: {}", page.lines().find(|l| l.contains("PHP ")).unwrap_or_default().trim());
            assert!(page.contains("Stackyard is running"), "unexpected page:\n{page}");

            // Virtual hosts: print what each one serves (the body's last line).
            for site in &settings.sites {
                let page = get(&format!("{}.{}", site.name, crate::sites::TLD));
                let body = page.split("\r\n\r\n").nth(1).unwrap_or_default().trim();
                println!("{}: {} | {}", site.name, page.lines().next().unwrap_or_default(), body.lines().last().unwrap_or_default());
                assert!(!body.contains("Stackyard is running"), "{} served the www start page", site.name);
            }
        }

        let started = Instant::now();
        for id in &ids {
            services.stop(id);
        }
        wait_while(&services, Status::Stopping, Duration::from_secs(60));
        println!("stopped in {:?}", started.elapsed());
    }
}
