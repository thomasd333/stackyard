//! "Open terminal": a shell in `www\` whose PATH starts with the selected tool versions.
//!
//! The PATH is set by small scripts in `config\terminal\` that the shell runs at startup, not by
//! inheritance alone: Windows Terminal may open the tab from an already running process that has its own
//! environment. The scripts find the root relative to themselves, so they don't embed absolute paths.

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::System::Threading::{CREATE_DEFAULT_ERROR_MODE, CREATE_NEW_CONSOLE};

use crate::env_path;
use crate::paths;
use crate::settings::Terminal;
use crate::tools::ToolDef;

pub const SHELLS: &[&str] = &["cmd", "powershell", "bash", "custom"];

/// Windows Terminal's app execution alias. It is a reparse point that `is_file()` may not follow.
pub fn windows_terminal() -> Option<PathBuf> {
    let wt = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join(r"Microsoft\WindowsApps\wt.exe");
    std::fs::symlink_metadata(&wt).is_ok().then_some(wt)
}

/// Git for Windows install dir (the one with `bin\bash.exe`).
pub fn git_bash(git_dir: Option<&Path>) -> Option<PathBuf> {
    git_dir.filter(|d| d.join(r"bin\bash.exe").is_file()).map(Path::to_path_buf)
}

/// One selected tool version: display text and its PATH dirs.
pub struct ToolDirs {
    pub label: String,
    pub dirs: Vec<PathBuf>,
}

impl ToolDirs {
    pub fn new(tool: &ToolDef, label: &str, install_dir: &Path) -> Self {
        Self { label: format!("{} {label}", tool.name), dirs: tool.path_dirs_for(install_dir) }
    }
}

/// Opens the configured terminal. `git_dir` is the selected Git install, needed for the bash shell.
pub fn open(cfg: &Terminal, tools: &[ToolDirs], git_dir: Option<&Path>) -> Result<String, String> {
    let root = paths::app_root();
    let www = paths::www_dir();
    let scripts = Scripts::new(tools)?;

    let wt = if cfg.windows_terminal { windows_terminal() } else { None };
    let system_root = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()));
    let fresh_path = env_path::current_full_path();

    let mut cmd = match cfg.shell.as_str() {
        "custom" => {
            let program = PathBuf::from(cfg.custom.trim().trim_matches('"'));
            if cfg.custom.trim().is_empty() {
                return Err("Choose the terminal program first (e.g. C:\\tools\\cmder\\Cmder.exe).".into());
            }
            if !program.is_file() {
                return Err(format!("Terminal program not found: {}", program.display()));
            }
            // A custom terminal gets the PATH by inheritance; there is no script it would run.
            let ours: Vec<String> = scripts.rel.iter().map(|d| root.join(d).to_string_lossy().into_owned()).collect();
            let mut c = Command::new(&program);
            c.env("PATH", format!("{};{fresh_path}", ours.join(";")));
            c.current_dir(&www).creation_flags(CREATE_DEFAULT_ERROR_MODE);
            spawn(c, &program)?;
            return Ok(format!("custom ({})", program.display()));
        }
        "powershell" => {
            let script = scripts.write("env.ps1", ps1_script)?;
            let pwsh = env_path::split_find(&fresh_path, "pwsh.exe");
            let exe = pwsh.unwrap_or_else(|| system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
            let mut c = launcher(wt.as_deref(), &exe, &www);
            c.args(["-NoLogo", "-NoExit", "-ExecutionPolicy", "Bypass", "-File"]).arg(script);
            c
        }
        "bash" => {
            let git = git_bash(git_dir).ok_or("Git Bash needs Git in bin\\git\\. Choose another shell or install Git.")?;
            let script = scripts.write("env.sh", sh_script)?;
            let script = paths::fwd(&script);
            let mintty = git.join(r"usr\bin\mintty.exe");
            if wt.is_none() && mintty.is_file() {
                // Git Bash's own window; MSYSTEM makes /etc/profile add mingw64\bin like git-bash.exe does.
                let mut c = Command::new(&mintty);
                c.args(["-t", "Stackyard", "/usr/bin/bash", "--rcfile", &script, "-i"]);
                c.env("MSYSTEM", "MINGW64").current_dir(&www);
                c
            } else {
                // bin\bash.exe sets MSYSTEM and the MSYS PATH itself.
                let mut c = launcher(wt.as_deref(), &git.join(r"bin\bash.exe"), &www);
                c.args(["--rcfile", &script, "-i"]);
                c
            }
        }
        _ => {
            let script = scripts.write("env.cmd", cmd_script)?;
            let comspec = std::env::var_os("ComSpec").map(PathBuf::from);
            let exe = comspec.unwrap_or_else(|| system_root.join(r"System32\cmd.exe"));
            // "call" keeps cmd from stripping the quotes around a path with spaces.
            let mut c = launcher(wt.as_deref(), &exe, &www);
            c.args(["/k", "call"]).arg(script);
            c
        }
    };
    cmd.env("PATH", &fresh_path).creation_flags(CREATE_NEW_CONSOLE | CREATE_DEFAULT_ERROR_MODE);
    let program = PathBuf::from(cmd.get_program());
    spawn(cmd, &program)?;
    Ok(format!("{}{}", cfg.shell, if wt.is_some() { " in Windows Terminal" } else { "" }))
}

/// Writes the startup scripts into `config\terminal\`.
struct Scripts {
    dir: PathBuf,
    /// Tool dirs relative to the root, in PATH order (first wins).
    rel: Vec<String>,
    banner: String,
}

impl Scripts {
    fn new(tools: &[ToolDirs]) -> Result<Self, String> {
        let root = paths::app_root();
        let dir = paths::config_dir().join("terminal");
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let rel = tools
            .iter()
            .flat_map(|t| &t.dirs)
            .filter_map(|d| d.strip_prefix(&root).ok())
            .map(|d| d.to_string_lossy().into_owned())
            .collect();
        let banner = format!("Stackyard: {}", tools.iter().map(|t| t.label.as_str()).collect::<Vec<_>>().join(", "));
        Ok(Self { dir, rel, banner })
    }

    fn write(&self, name: &str, script: fn(&[String], &str) -> String) -> Result<PathBuf, String> {
        let path = self.dir.join(name);
        std::fs::write(&path, script(&self.rel, &self.banner)).map_err(|e| format!("Cannot write {name}: {e}"))?;
        Ok(path)
    }
}

/// `exe` directly in a new console, or as a Windows Terminal tab starting in `dir`.
fn launcher(wt: Option<&Path>, exe: &Path, dir: &Path) -> Command {
    match wt {
        Some(wt) => {
            let mut c = Command::new(wt);
            c.arg("-d").arg(dir).arg(exe);
            c
        }
        None => {
            let mut c = Command::new(exe);
            c.current_dir(dir);
            c
        }
    }
}

fn spawn(mut cmd: Command, program: &Path) -> Result<(), String> {
    // Don't keep a handle to the terminal: it outlives Stackyard.
    cmd.spawn().map(drop).map_err(|e| format!("Cannot start {}: {e}", program.display()))
}

/// Escapes text for `echo` in a batch file.
fn cmd_echo(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '%' => vec!['%', '%'],
            '^' | '&' | '|' | '<' | '>' | '(' | ')' => vec!['^', c],
            _ => vec![c],
        })
        .collect()
}

fn cmd_script(rel: &[String], banner: &str) -> String {
    let dirs: String = rel.iter().map(|d| format!("%%~fr\\{};", d.replace('%', "%%"))).collect();
    format!(
        "@echo off\r\n\
         rem Generated by Stackyard each time a terminal opens; edits are overwritten.\r\n\
         for %%r in (\"%~dp0..\\..\") do set \"PATH={dirs}%PATH%\"\r\n\
         title Stackyard\r\n\
         echo {}\r\n",
        cmd_echo(banner)
    )
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn ps1_script(rel: &[String], banner: &str) -> String {
    let dirs: Vec<String> = rel.iter().map(|d| ps_quote(d)).collect();
    // BOM: Windows PowerShell 5.1 reads BOM-less scripts in the ANSI code page.
    format!(
        "\u{feff}# Generated by Stackyard each time a terminal opens; edits are overwritten.\r\n\
         $root = Split-Path (Split-Path $PSScriptRoot)\r\n\
         $env:Path = ((@({}) | ForEach-Object {{ Join-Path $root $_ }}) + $env:Path) -join ';'\r\n\
         $Host.UI.RawUI.WindowTitle = 'Stackyard'\r\n\
         Write-Host {}\r\n",
        dirs.join(", "),
        ps_quote(banner)
    )
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn sh_script(rel: &[String], banner: &str) -> String {
    // Prepended one by one, so iterate in reverse to keep the first dir first.
    let dirs: Vec<String> = rel.iter().rev().map(|d| sh_quote(&d.replace('\\', "/"))).collect();
    format!(
        "# Generated by Stackyard each time a terminal opens; edits are overwritten.\n\
         # Started with --rcfile, so do what a login shell would: /etc/profile, then ~/.bashrc.\n\
         . /etc/profile\n\
         [ -f ~/.bashrc ] && . ~/.bashrc\n\
         __root=\"$(cd \"$(dirname \"${{BASH_SOURCE[0]}}\")/../..\" && pwd)\"\n\
         for __d in {}; do PATH=\"$__root/$__d:$PATH\"; done\n\
         export PATH\n\
         unset __root __d\n\
         echo {}\n",
        dirs.join(" "),
        sh_quote(banner)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel() -> Vec<String> {
        vec![r"bin\php\php-8.3".into(), r"bin\it's (x86) 100%".into()]
    }

    #[test]
    fn cmd_script_is_relative_and_escaped() {
        let s = cmd_script(&rel(), "Stackyard: PHP & co (100%)");
        assert!(s.contains(r#"set "PATH=%%~fr\bin\php\php-8.3;%%~fr\bin\it's (x86) 100%%;%PATH%""#));
        assert!(s.contains("echo Stackyard: PHP ^& co ^(100%%^)"));
        assert!(!s.contains("SERVER"));
    }

    #[test]
    fn ps1_script_quotes() {
        let s = ps1_script(&rel(), "a'b");
        assert!(s.starts_with('\u{feff}'));
        assert!(s.contains(r"@('bin\php\php-8.3', 'bin\it''s (x86) 100%')"));
        assert!(s.contains("Write-Host 'a''b'"));
    }

    #[test]
    fn sh_script_keeps_order() {
        let s = sh_script(&rel(), "x");
        assert!(s.contains(r"for __d in 'bin/it'\''s (x86) 100%' 'bin/php/php-8.3'; do"));
        assert!(!s.contains('\r'));
    }

    /// Writes the scripts into the real `config\terminal\` and runs each shell without a window, checking
    /// that php resolves under our bin\: `cargo test terminal_scripts_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn terminal_scripts_real() {
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        let bin = paths::bin_dir();
        let settings = crate::settings::Settings::load();
        let selected = |t: &ToolDef| crate::commands::selected_install(t, t.installs(&bin), &settings);
        let tools: Vec<ToolDirs> = crate::tools::TOOLS
            .iter()
            .filter_map(|t| selected(t).map(|i| ToolDirs::new(t, &i.label, &i.dir)))
            .collect();
        let scripts = Scripts::new(&tools).unwrap();
        let bin_prefix = bin.to_string_lossy().to_lowercase();
        let run = |name: &str, mut c: Command| {
            let out = c.env("PATH", env_path::current_full_path()).creation_flags(CREATE_NO_WINDOW).output().unwrap();
            let text = String::from_utf8_lossy(&out.stdout);
            println!("--- {name} ---\n{text}{}", String::from_utf8_lossy(&out.stderr));
            let php: Vec<&str> = text.lines().filter(|l| l.to_lowercase().ends_with("php.exe")).collect();
            assert!(!php.is_empty() && php[0].to_lowercase().starts_with(&bin_prefix), "{name}: php not from bin");
        };

        let cmd = scripts.write("env.cmd", cmd_script).unwrap();
        let mut c = Command::new("cmd.exe");
        c.args(["/c", "call"]).arg(&cmd).args(["&", "where", "php", "node"]);
        run("cmd", c);

        let ps1 = scripts.write("env.ps1", ps1_script).unwrap();
        let mut c = Command::new("powershell.exe");
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
            .arg(format!(". '{}'; (Get-Command php, node).Source", ps1.display()));
        run("powershell", c);

        let git = crate::tools::find("git").and_then(selected);
        if let Some(git) = git_bash(git.as_ref().map(|g| g.dir.as_path())) {
            let sh = paths::fwd(&scripts.write("env.sh", sh_script).unwrap());
            let mut c = Command::new(git.join(r"bin\bash.exe"));
            c.args(["-c", &format!(r#". '{sh}'; cygpath -w "$(command -v php)" "$(command -v node)""#)]);
            run("bash", c);
        }
    }
}
