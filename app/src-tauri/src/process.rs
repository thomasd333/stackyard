//! Child process helpers: hidden spawning, Job Objects and graceful shutdown signals.

use std::fs::File;
use std::io;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, EVENT_MODIFY_STATE, OpenEventW, SetEvent};

/// A Job Object that kills all its processes when closed - including when Stackyard itself
/// exits or crashes, so no orphaned httpd.exe / mysqld.exe are left behind.
pub struct Job(HANDLE);

// The handle is only used through thread-safe Win32 calls.
unsafe impl Send for Job {}
unsafe impl Sync for Job {}

impl Job {
    pub fn new() -> io::Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Job(handle);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }

    pub fn assign(&self, child: &Child) -> io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Kills every process in the job (the whole process tree).
    pub fn terminate(&self) {
        unsafe { TerminateJobObject(self.0, 1) };
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

/// How to ask a service to shut down cleanly before resorting to killing it.
pub enum Stop {
    /// Named event set to request shutdown (Apache's `ap<pid>_shutdown`).
    Event(String),
    /// Commands requesting shutdown (e.g. `mysqladmin shutdown`), tried in order until one succeeds.
    Commands(Vec<Command>),
}

/// A running service process together with the job that owns its process tree.
pub struct Proc {
    pub child: Child,
    job: Job,
    stop: Option<Stop>,
}

impl Proc {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Exit code if the process has ended.
    pub fn exited(&mut self) -> Option<Option<i32>> {
        self.child.try_wait().ok().flatten().map(|s| s.code())
    }

    /// Waits up to `timeout` for the process to exit; true if it did.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.exited().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    pub fn set_stop(&mut self, stop: Stop) {
        self.stop = Some(stop);
    }

    /// Requests a clean shutdown, then kills the tree if it is still alive after `timeout`.
    /// Returns true if the process stopped on its own (false = it had to be killed).
    /// `timeout` covers the stop commands too: `mysqladmin shutdown` waits for the server, up to an hour.
    pub fn shutdown(mut self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let left = || deadline.saturating_duration_since(Instant::now());
        let requested = match self.stop.take() {
            Some(Stop::Event(name)) => signal_event(&name),
            Some(Stop::Commands(cmds)) => cmds.into_iter().any(|mut cmd| run_quiet(&mut cmd, left())),
            None => false,
        };
        if requested && self.wait_for_exit(left()) {
            return true;
        }
        self.kill();
        false
    }

    pub fn kill(mut self) {
        self.job.terminate();
        let _ = self.child.wait();
    }
}

/// Starts `cmd` without a console window, with stdout+stderr going to `log`, inside a new job.
pub fn spawn(cmd: &mut Command, log: &Path) -> io::Result<Proc> {
    let job = Job::new()?;
    let out = File::create(log)?;
    let child = cmd
        .stdin(Stdio::null())
        .stdout(out.try_clone()?)
        .stderr(out)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    if let Err(e) = job.assign(&child) {
        let mut child = child;
        let _ = child.kill();
        return Err(e);
    }
    Ok(Proc { child, job, stop: None })
}

/// Runs `cmd` without a window or output and waits up to `timeout`. True if it exited successfully in
/// time; one that takes longer is killed.
pub fn run_quiet(cmd: &mut Command, timeout: Duration) -> bool {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
    let Ok(mut child) = cmd.spawn() else { return false };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Sets a named event; false if it does not exist.
fn signal_event(name: &str) -> bool {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let handle = OpenEventW(EVENT_MODIFY_STATE, 0, wide.as_ptr());
        if handle.is_null() {
            return false;
        }
        let ok = SetEvent(handle) != 0;
        CloseHandle(handle);
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that would run for ~10 s.
    fn slow() -> Command {
        let mut cmd = Command::new("ping");
        cmd.args(["-n", "10", "127.0.0.1"]);
        cmd
    }

    #[test]
    fn run_quiet_kills_a_command_that_takes_too_long() {
        let started = Instant::now();
        assert!(!run_quiet(&mut slow(), Duration::from_millis(300)));
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    }

    #[test]
    fn run_quiet_reports_the_exit_status() {
        assert!(run_quiet(Command::new("cmd").args(["/c", "exit 0"]), Duration::from_secs(10)));
        assert!(!run_quiet(Command::new("cmd").args(["/c", "exit 3"]), Duration::from_secs(10)));
        assert!(!run_quiet(&mut Command::new("does-not-exist-stackyard.exe"), Duration::from_secs(10)));
    }

    #[test]
    fn shutdown_is_bounded_by_its_timeout_even_when_the_stop_command_hangs() {
        let log = std::env::temp_dir().join(format!("stackyard-proc-{}.log", std::process::id()));
        let mut proc = spawn(&mut slow(), &log).unwrap();
        proc.set_stop(Stop::Commands(vec![slow(), slow()]));
        let started = Instant::now();
        assert!(!proc.shutdown(Duration::from_millis(500)), "the process should have been killed");
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
        let _ = std::fs::remove_file(log);
    }
}
