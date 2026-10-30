//! Starting, stopping and supervising Apache, MySQL and PostgreSQL.
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
use crate::settings::{self, Settings};
use crate::tools::{self, Install};

pub const SERVICES: &[(&str, &str)] =
    &[("apache", "Apache"), ("mysql", "MySQL"), ("postgresql", "PostgreSQL"), ("mailpit", "Mailpit")];

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
    /// Stop was asked for while starting; the start thread stops the service as soon as it is up.
    cancel: bool,
}

/// How long a database may take to accept connections. Generous on purpose: crash recovery or an
/// in-place upgrade of a big datadir takes minutes, and killing it then does real damage.
const DB_START_TIMEOUT: Duration = Duration::from_secs(300);

/// How long quitting waits for services that are still starting or stopping.
const EXIT_WAIT: Duration = Duration::from_secs(60);

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
        // "Start all" and restoring skip what isn't installed (the buttons for it are disabled anyway).
        if selected(id, &Settings::load()).is_none() {
            return;
        }
        {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            reap(id, slot);
            if slot.status == Status::Starting && slot.cancel {
                // Started again before the stop asked for during the start took effect.
                slot.cancel = false;
                drop(slots);
                remember(id, true);
                return;
            }
            if matches!(slot.status, Status::Starting | Status::Running | Status::Stopping) {
                return;
            }
            *slot = Slot { status: Status::Starting, ..Slot::default() };
        }

        applog::info(format!("{id}: starting"));
        remember(id, true);
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let result = me.launch(id);
            let mut slots = me.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            let next = match result {
                Ok((proc, _)) if slot.cancel => {
                    slot.status = Status::Stopping;
                    drop(slots);
                    applog::info(format!("{id}: stop was requested during the start"));
                    shutdown(id, proc);
                    me.reset(id);
                    return;
                }
                Ok((proc, port)) => {
                    applog::info(format!("{id}: running (PID {}, port {port})", proc.pid()));
                    Slot { status: Status::Running, proc: Some(proc), port: Some(port), ..Slot::default() }
                }
                Err(msg) => {
                    applog::error(format!("{id}: start failed: {msg}"));
                    Slot { status: Status::Error, message: Some(msg), ..Slot::default() }
                }
            };
            *slot = next;
        });
    }

    pub fn stop(self: &Arc<Self>, id: &'static str) {
        remember(id, false);
        let Some(proc) = self.take_for_stop(id) else {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            match slot.status {
                // Stopping a failed service just clears its error.
                Status::Error => *slot = Slot::default(),
                // Not up yet: the start thread stops it once it is (killing it mid-start could leave a
                // half-initialised datadir).
                Status::Starting => slot.cancel = true,
                _ => {}
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

    /// Gracefully stops everything and waits for it; used when the app exits. Services that are still
    /// starting or stopping on another thread are waited for (up to `EXIT_WAIT`): exiting closes their
    /// Job Objects, which would kill e.g. MySQL in the middle of a clean shutdown.
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
        let deadline = Instant::now() + EXIT_WAIT;
        while self.cancel_transitions() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Asks every service that is starting to stop once it is up; true while any is starting or stopping.
    fn cancel_transitions(&self) -> bool {
        let mut busy = false;
        for slot in self.slots.lock().unwrap().values_mut() {
            slot.cancel |= slot.status == Status::Starting;
            busy |= matches!(slot.status, Status::Starting | Status::Stopping);
        }
        busy
    }

    fn cancelled(&self, id: &'static str) -> bool {
        self.slots.lock().unwrap().get(id).is_some_and(|s| s.cancel)
    }

    fn set_message(&self, id: &'static str, message: &str) {
        self.slots.lock().unwrap().entry(id).or_default().message = Some(message.to_string());
    }

    /// Waits until the service accepts connections; on failure kills it and reports the log tail.
    /// Returns early (with the process) when a stop is requested meanwhile.
    fn wait_ready(&self, id: &'static str, mut proc: Proc, port: u16, timeout: Duration, logs: &[&Path]) -> StartResult {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(code) = proc.exited() {
                return Err(explain_exit(code, &format!("Process exited during startup.\n{}", tail(logs, 6))));
            }
            if ports::is_listening(port) || self.cancelled(id) {
                return Ok((proc, port));
            }
            if Instant::now() > deadline {
                proc.kill();
                return Err(format!("Not listening on port {port} after {}s.\n{}", timeout.as_secs(), tail(logs, 6)));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
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
                "Port {port} is already in use by {}.\nStop that program or change the port in Settings > Ports.",
                ports::describe_owner(port)
            ));
        }
        match id {
            "apache" => self.launch_apache(&settings, port),
            "mysql" => self.launch_mysql(&settings, port),
            "postgresql" => self.launch_postgresql(&settings, port),
            "mailpit" => self.launch_mailpit(&settings, port),
            _ => Err(format!("Unknown service: {id}")),
        }
    }

    fn launch_apache(&self, settings: &Settings, port: u16) -> StartResult {
        let apache = selected("apache", settings).ok_or("Apache is not installed (bin\\apache).")?;
        let php = selected("php", settings);
        // Apache's port comes from the caller (checked free); the others are only for Adminer's defaults.
        let ports = settings::Ports { apache: port, ..settings.ports };
        let conf = confgen::write_apache(&apache.dir, php.as_ref().map(|p| p.dir.as_path()), &ports, settings)?;
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
        self.wait_ready("apache", proc, port, Duration::from_secs(15), &[&log, &paths::logs_dir().join("apache_error.log")])
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
        let ready = self.wait_ready("mysql", proc, port, DB_START_TIMEOUT, &[&log, &logs.join("mysql_error.log")])?;
        if !self.cancelled("mysql") {
            ensure_shutdown_account(&mysql.dir, port);
        }
        Ok(ready)
    }
}

impl Services {
    fn launch_postgresql(&self, settings: &Settings, port: u16) -> StartResult {
        let pg = selected("postgresql", settings).ok_or("PostgreSQL is not installed (bin\\postgresql).")?;
        let major = major_version(&pg.label);
        let datadir = postgresql_datadir(&pg.label);
        let bin = pg.dir.join("bin");
        let logs = paths::logs_dir();
        applog::info(format!("postgresql: {} on port {port}, datadir {}", pg.label, datadir.display()));

        if !datadir.join("PG_VERSION").exists() {
            applog::info("postgresql: initializing a new data directory");
            self.set_message("postgresql", "Initializing data directory (first start)…");
            let log = logs.join("postgresql_init.log");
            let mut cmd = Command::new(bin.join("initdb.exe"));
            // Superuser "postgres" without a password, trusted from this computer only (initdb's
            // pg_hba.conf with -A trust lists only 127.0.0.1 and ::1). UTF-8 and no Windows code page
            // locale, so the cluster behaves the same on every machine.
            cmd.arg("-D").arg(&datadir).args(["-U", "postgres", "-A", "trust", "-E", "UTF8", "--locale=C"]);
            if major.is_some_and(|m| m >= 17) {
                // Unicode-aware upper()/lower() and ILIKE, still independent of the OS.
                cmd.args(["--locale-provider=builtin", "--builtin-locale=C.UTF-8"]);
            }
            let mut proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot run initdb: {e}"))?;
            if !proc.wait_for_exit(Duration::from_secs(180)) {
                proc.kill();
                return Err("PostgreSQL initialization timed out.".into());
            }
            let code = proc.exited().flatten();
            if code != Some(0) {
                return Err(explain_exit(code, &format!("PostgreSQL initialization failed.\n{}", tail(&[&log], 6))));
            }
            self.set_message("postgresql", "Starting…");
        }

        let log = logs.join("postgresql.log");
        let mut cmd = Command::new(bin.join("postgres.exe"));
        cmd.arg("-D").arg(&datadir).args(["-p", &port.to_string(), "-c", "listen_addresses=localhost"]);
        let mut proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot start PostgreSQL: {e}"))?;
        // "fast": roll back open transactions and stop, which is what a dev server wants.
        let mut stop = Command::new(bin.join("pg_ctl.exe"));
        stop.arg("stop").arg("-D").arg(&datadir).args(["-m", "fast", "-w", "-t", "30"]);
        proc.set_stop(Stop::Commands(vec![stop]));
        self.wait_ready("postgresql", proc, port, DB_START_TIMEOUT, &[&log])
    }

    /// Mailpit: catches what PHP's mail() (or an app's SMTP settings) sends, shown in a web inbox.
    fn launch_mailpit(&self, settings: &Settings, port: u16) -> StartResult {
        let mailpit = selected("mailpit", settings).ok_or("Mailpit is not installed (bin\\mailpit).")?;
        let smtp = settings.ports.smtp;
        if !ports::is_free(smtp) {
            return Err(format!(
                "SMTP port {smtp} is already in use by {}.\nStop that program or change the port in Settings > Ports.",
                ports::describe_owner(smtp)
            ));
        }
        // Messages are kept across restarts (Mailpit itself trims them to the newest 500).
        let data = paths::data_dir().join("mailpit");
        std::fs::create_dir_all(&data).map_err(|e| format!("Cannot create {}: {e}", data.display()))?;
        applog::info(format!("mailpit: {} on port {port}, SMTP {smtp}", mailpit.label));
        let log = paths::logs_dir().join("mailpit.log");
        let mut cmd = Command::new(mailpit.dir.join("mailpit.exe"));
        cmd.arg("--listen")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--smtp")
            .arg(format!("127.0.0.1:{smtp}"))
            .arg("--database")
            .arg(data.join("mailpit.db"))
            // No network unless the user asks (Stackyard's own policy).
            .arg("--disable-version-check");
        let proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot start Mailpit: {e}"))?;
        self.wait_ready("mailpit", proc, port, Duration::from_secs(30), &[&log])
    }

    /// Dumps the databases of a running MySQL or PostgreSQL into `backup\databases\` and returns the
    /// file. MySQL: every database except the system ones (so it loads into another MySQL version);
    /// PostgreSQL: the whole cluster with its roles (pg_dumpall).
    pub fn backup(&self, id: &'static str) -> Result<PathBuf, String> {
        let settings = Settings::load();
        let name = SERVICES.iter().find(|(s, _)| *s == id).map(|(_, n)| *n).ok_or("Only databases can be backed up.")?;
        let port = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.entry(id).or_default();
            reap(id, slot);
            if slot.status != Status::Running {
                return Err(format!("Start {name} first: the backup is made from the running server."));
            }
            slot.port.unwrap_or_else(|| configured_port(id, &settings))
        };
        let install = selected(id, &settings).ok_or_else(|| format!("{name} is not installed."))?;
        let datadir = datadir(id, &install.label).ok_or("Only databases can be backed up.")?;
        let dir = paths::backups_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let stem = datadir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| id.into());
        let file = dir.join(format!("{stem}-{}.sql", applog::file_stamp()));
        let bin = install.dir.join("bin");
        let port_arg = format!("--port={port}");

        let mut cmd = if id == "mysql" {
            let databases = mysql_databases(&bin, port)?;
            if databases.is_empty() {
                return Err("There is nothing to back up: MySQL has only its system databases.".into());
            }
            let mut cmd = Command::new(bin.join("mysqldump.exe"));
            // (mysqldump has no --connect-timeout; mysql_databases just reached the server.)
            cmd.args(["--user=root", "--host=127.0.0.1", &port_arg])
                .args(["--single-transaction", "--routines", "--events", "--triggers", "--default-character-set=utf8mb4"])
                .arg(format!("--result-file={}", file.display()))
                .arg("--databases")
                .args(databases);
            cmd
        } else {
            let mut cmd = Command::new(bin.join("pg_dumpall.exe"));
            cmd.args(["-h", "127.0.0.1", "-p", &port.to_string(), "-U", "postgres", "-f"]).arg(&file);
            cmd
        };
        applog::info(format!("{id}: backing up to {}", file.display()));
        let log = paths::logs_dir().join("backup.log");
        let mut proc = process::spawn(&mut cmd, &log).map_err(|e| format!("Cannot run the backup: {e}"))?;
        let finished = proc.wait_for_exit(BACKUP_TIMEOUT);
        let code = proc.exited().flatten();
        if !finished || code != Some(0) {
            if !finished {
                proc.kill();
            }
            let _ = std::fs::remove_file(&file);
            let reason = if finished { tail(&[&log], 6) } else { format!("Not finished after {} minutes.", BACKUP_TIMEOUT.as_secs() / 60) };
            applog::error(format!("{id}: backup failed: {reason}"));
            let hint = if reason.contains("Access denied") {
                "\nroot has a password: export the databases with Adminer instead."
            } else {
                ""
            };
            return Err(format!("The backup failed.\n{reason}{hint}"));
        }
        applog::info(format!("{id}: backup saved ({} bytes)", std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0)));
        Ok(file)
    }
}

/// How long a database backup may take.
const BACKUP_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// MySQL's databases without the system ones, as root without a password.
fn mysql_databases(bin: &Path, port: u16) -> Result<Vec<String>, String> {
    const SYSTEM: &[&str] = &["information_schema", "mysql", "performance_schema", "sys"];
    let out = Command::new(bin.join("mysql.exe"))
        .args(["--user=root", "--host=127.0.0.1", "--connect-timeout=5", &format!("--port={port}")])
        .args(["--batch", "--skip-column-names", "-e", "SHOW DATABASES"])
        .stdin(std::process::Stdio::null())
        .creation_flags_hidden()
        .output()
        .map_err(|e| format!("Cannot run mysql.exe: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let hint = if err.contains("Access denied") { "\nroot has a password: export the databases with Adminer instead." } else { "" };
        return Err(format!("Cannot list the databases: {err}{hint}"));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|db| !db.is_empty() && !SYSTEM.contains(&db.to_lowercase().as_str()))
        .map(str::to_string)
        .collect())
}

/// Data directory of a database version: MySQL per major.minor, PostgreSQL per major (neither can read
/// an older format without an upgrade step). None for other tools.
pub fn datadir(id: &str, label: &str) -> Option<PathBuf> {
    match id {
        "mysql" => Some(confgen::mysql_datadir(label)),
        "postgresql" => Some(postgresql_datadir(label)),
        _ => None,
    }
}

/// True when the data directory has been initialised (it holds databases, at least the system ones).
pub fn has_data(id: &str, dir: &Path) -> bool {
    dir.join(if id == "mysql" { "mysql.ibd" } else { "PG_VERSION" }).exists()
}

fn postgresql_datadir(label: &str) -> PathBuf {
    paths::data_dir().join(major_version(label).map_or_else(|| label.to_string(), |m| format!("postgresql-{m}")))
}

/// The major version from an install folder name, e.g. 18 for `postgresql-18.6`.
fn major_version(label: &str) -> Option<u32> {
    label.split(|c: char| !c.is_ascii_digit()).find(|s| !s.is_empty())?.parse().ok()
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

/// Runs a mysql.exe query; the timeout covers a server that accepts connections but doesn't answer.
fn hidden_status(cmd: &mut Command) -> bool {
    process::run_quiet(cmd, Duration::from_secs(20))
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

/// Records that the user left `id` running (or stopped it), for "restore services at launch".
/// `stop_all_blocking` on quit doesn't come through here, so quitting keeps the list.
fn remember(id: &str, running: bool) {
    if cfg!(test) {
        return;
    }
    if Settings::load().running.iter().any(|x| x == id) == running {
        return;
    }
    // Runs on service threads too (a restart), concurrently with commands that save.
    let saved = Settings::update(|s| {
        s.running.retain(|x| x != id);
        if running {
            s.running.push(id.to_string());
        }
    });
    if let Err(e) = saved {
        applog::warn(format!("Cannot save the running services: {e}"));
    }
}

fn configured_port(id: &str, settings: &Settings) -> u16 {
    match id {
        "mysql" => settings.ports.mysql,
        "postgresql" => settings.ports.postgresql,
        "mailpit" => settings.ports.mailpit,
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
    let log = logs.join(match id {
        "mysql" => "mysql_error.log",
        "postgresql" => "postgresql.log",
        "mailpit" => "mailpit.log",
        _ => "apache_error.log",
    });
    let message = explain_exit(code, &format!("Process exited unexpectedly.\n{}", tail(&[&log], 4)));
    applog::error(format!("{id}: exited unexpectedly (code {code:?}): {message}"));
    slot.message = Some(message);
}

fn shutdown(id: &str, proc: Proc) {
    // Mailpit has no stop command and nothing to flush (its SQLite database survives a kill).
    if id == "mailpit" {
        proc.kill();
        applog::info(format!("{id}: stopped"));
        return;
    }
    // The databases may need a checkpoint first; pg_ctl itself waits up to 30 s (`-t 30`).
    if proc.shutdown(Duration::from_secs(if id == "apache" { 10 } else { 30 })) {
        applog::info(format!("{id}: stopped"));
    } else {
        applog::warn(format!("{id}: did not stop cleanly, process tree was killed"));
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
        .filter_map(|f| read_end(f, TAIL_BYTES))
        .flat_map(|bytes| String::from_utf8_lossy(&bytes).lines().map(str::to_string).collect::<Vec<_>>())
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// How much of the end of a log `tail` reads; error logs grow to 10 MB before they are rotated.
const TAIL_BYTES: u64 = 64 * 1024;

/// The last `max` bytes of a file.
fn read_end(path: &Path, max: u64) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(bytes)
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

    #[test]
    fn postgresql_major_version() {
        assert_eq!(major_version("postgresql-18.6"), Some(18));
        assert_eq!(major_version("17.11"), Some(17));
        assert_eq!(major_version("default"), None);
    }

    #[test]
    fn tail_reads_only_the_end_of_a_big_log() {
        let path = std::env::temp_dir().join(format!("stackyard-tail-{}.log", std::process::id()));
        let mut text = "an old line that should not be read\n".repeat(10_000);
        text.push_str("last but one\n\n  \nlast\n");
        std::fs::write(&path, &text).unwrap();
        assert!(text.len() as u64 > 4 * TAIL_BYTES);
        assert_eq!(tail(&[&path], 2), "last but one\nlast");
        assert_eq!(read_end(&path, TAIL_BYTES).unwrap().len() as u64, TAIL_BYTES);
        assert_eq!(tail(&[&path.with_extension("missing")], 2), "");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn stop_while_starting_is_kept_until_the_start_ends() {
        let services = Arc::new(Services::default());
        let set = |status| services.slots.lock().unwrap().entry("apache").or_default().status = status;
        set(Status::Starting);
        services.stop("apache");
        assert!(services.cancelled("apache"), "the stop must not be lost");
        assert!(services.snapshot(&Settings::default()).iter().any(|s| s.id == "apache" && s.status == Status::Starting));
        // Quitting waits while anything is starting or stopping, and asks starting services to stop.
        set(Status::Stopped);
        services.slots.lock().unwrap().entry("mysql").or_default().status = Status::Starting;
        assert!(services.cancel_transitions());
        assert!(services.cancelled("mysql"));
        services.slots.lock().unwrap().entry("mysql").or_default().status = Status::Stopped;
        assert!(!services.cancel_transitions());
    }

    /// A stop asked for during the start ends with the service stopped, not running:
    /// `cargo test stop_during_start_real -- --ignored --nocapture` (uses the Apache port from settings).
    #[test]
    #[ignore]
    fn stop_during_start_real() {
        let port = Settings::load().ports.apache;
        let services = Arc::new(Services::default());
        services.start("apache");
        services.stop("apache");
        wait_while(&services, Status::Starting, Duration::from_secs(30));
        let snap = wait_while(&services, Status::Stopping, Duration::from_secs(30));
        let apache = snap.iter().find(|s| s.id == "apache").unwrap();
        println!("apache {:?} pid={:?} msg={:?}", apache.status as u8, apache.pid, apache.message);
        assert!(apache.status == Status::Stopped && apache.pid.is_none());
        assert!(ports::is_free(port), "something still listens on {port}");
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
        let wanted = std::env::var("STACKYARD_TEST_SERVICES").unwrap_or_else(|_| "apache,mysql,postgresql,mailpit".into());
        let settings = Settings::load();
        // Services that aren't installed (e.g. no PostgreSQL) are skipped.
        let ids: Vec<&'static str> = SERVICES
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| wanted.split(',').any(|w| w == *id) && selected(id, &settings).is_some())
            .collect();
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
        let request = |raw: String| {
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", settings.ports.apache)).unwrap();
            conn.write_all(raw.as_bytes()).unwrap();
            let mut page = String::new();
            conn.read_to_string(&mut page).unwrap();
            page
        };
        let get = |host: &str| request(format!("GET / HTTP/1.0\r\nHost: {host}\r\n\r\n"));

        // Adminer: log in without a password and an empty server (= our own port), then list databases.
        // Like a browser: open the login form (session cookies), post it, follow the redirect.
        // Cookie jar: a later Set-Cookie replaces an earlier one of the same name (the session id changes
        // at login).
        let remember = |jar: &mut Vec<(String, String)>, reply: &str| {
            for value in reply.lines().filter_map(|l| l.strip_prefix("Set-Cookie: ")) {
                let pair = value.split(';').next().unwrap_or("").trim();
                if let Some((name, val)) = pair.split_once('=') {
                    jar.retain(|(n, _)| n != name);
                    jar.push((name.to_string(), val.to_string()));
                }
            }
        };
        let header = |jar: &[(String, String)]| jar.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; ");
        let adminer_login = |driver: &str, user: &str| {
            let mut jar = Vec::new();
            let form = request("GET /adminer/ HTTP/1.0\r\nHost: localhost\r\n\r\n".into());
            assert!(form.contains(r#"value="root""#), "the login form has no prefilled username:\n{form}");
            remember(&mut jar, &form);
            let cookie = header(&jar);
            // The form's CSRF token, e.g. <input type='hidden' name='token' value='141040:945246'>.
            let token = form.split("name='token' value='").nth(1).and_then(|r| r.split('\'').next()).unwrap_or_default();
            let token = token.replace(':', "%3A");
            let body = format!(
                "token={token}&auth%5Bdriver%5D={driver}&auth%5Bserver%5D=&auth%5Busername%5D={user}&auth%5Bpassword%5D=&auth%5Bdb%5D="
            );
            let reply = request(format!(
                "POST /adminer/ HTTP/1.0\r\nHost: localhost\r\nCookie: {cookie}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ));
            let Some(location) = reply.lines().find_map(|l| l.strip_prefix("Location: ")).map(str::trim) else {
                let error = reply.lines().find(|l| l.contains("class='error'") || l.contains("class=\"error\""));
                return format!("login failed: {}", error.unwrap_or("no redirect"));
            };
            // Absolute, root-relative or (Adminer's usual) relative to /adminer/, e.g. "?username=root".
            let path = match location.split_once("://") {
                Some((_, rest)) => rest[rest.find('/').unwrap_or(rest.len())..].to_string(),
                None if location.starts_with('/') => location.to_string(),
                None => format!("/adminer/{location}"),
            };
            remember(&mut jar, &reply);
            let page = request(format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nCookie: {}\r\n\r\n", header(&jar)));
            if page.is_empty() { format!("empty reply for {path}") } else { page }
        };
        let adminer_installed = confgen::adminer_file().is_some() && selected("php", &settings).is_some();
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

            for (service, driver, user, database) in
                [("mysql", "server", "root", "information_schema"), ("postgresql", "pgsql", "postgres", "template1")]
            {
                // An empty Username means the default superuser.
                for user in [user, ""] {
                    if adminer_installed && ids.contains(&service) {
                        let page = adminer_login(driver, user);
                        let ok = page.contains(database);
                        println!("Adminer {service} as {user:?}: {}", if ok { "logged in, databases listed" } else { "FAILED" });
                        assert!(ok, "Adminer could not log in to {service} as {user:?}:\n{}", &page[..page.len().min(3000)]);
                    }
                }
            }
        }

        // mail() from the PHP CLI (same php.ini as Apache's) lands in Mailpit.
        if ids.contains(&"mailpit")
            && let Some(php) = selected("php", &settings)
        {
            let subject = format!("Stackyard test {}", std::process::id());
            let sent = Command::new(php.dir.join("php.exe"))
                .args(["-r", &format!("var_dump(mail('dev@example.com', '{subject}', 'Hello'));")])
                .output()
                .unwrap();
            println!("mail(): {}", String::from_utf8_lossy(&sent.stdout).trim());
            let mut conn = std::net::TcpStream::connect(("127.0.0.1", settings.ports.mailpit)).unwrap();
            conn.write_all(b"GET /api/v1/messages?limit=5 HTTP/1.0\r\nHost: localhost\r\n\r\n").unwrap();
            let mut reply = String::new();
            conn.read_to_string(&mut reply).unwrap();
            assert!(reply.contains(&subject), "the message is not in Mailpit:\n{reply}");
            println!("Mailpit: received {subject:?}");
        }

        // Backups of the running databases (MySQL may have nothing but its system databases).
        for id in ["mysql", "postgresql"].into_iter().filter(|id| ids.contains(id)) {
            match services.backup(id) {
                Ok(file) => {
                    let size = std::fs::metadata(&file).unwrap().len();
                    println!("backup {id}: {} ({size} bytes)", file.display());
                    assert!(size > 0);
                    std::fs::remove_file(file).unwrap();
                }
                Err(e) if e.contains("nothing to back up") => println!("backup {id}: {e}"),
                Err(e) => panic!("backup of {id} failed: {e}"),
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
