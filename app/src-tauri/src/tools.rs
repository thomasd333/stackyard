use std::cmp::Ordering;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// Static description of a tool that can live under `bin/<id>/`.
pub struct ToolDef {
    pub id: &'static str,
    pub name: &'static str,
    /// Executable (relative to a version directory) used to recognise a valid install.
    pub exe: &'static str,
    /// Directories (relative to a version directory) added to PATH; "" means the version dir itself.
    pub path_dirs: &'static [&'static str],
    /// Arguments printing the version, or None if the tool can't report it on its own.
    pub version_args: Option<&'static [&'static str]>,
}

pub const TOOLS: &[ToolDef] = &[
    ToolDef { id: "apache", name: "Apache", exe: "bin/httpd.exe", path_dirs: &["bin"], version_args: Some(&["-v"]) },
    ToolDef { id: "php", name: "PHP", exe: "php.exe", path_dirs: &[""], version_args: Some(&["-v"]) },
    ToolDef { id: "mysql", name: "MySQL", exe: "bin/mysqld.exe", path_dirs: &["bin"], version_args: Some(&["--version"]) },
    ToolDef { id: "postgresql", name: "PostgreSQL", exe: "bin/postgres.exe", path_dirs: &["bin"], version_args: Some(&["--version"]) },
    ToolDef { id: "nodejs", name: "Node.js", exe: "node.exe", path_dirs: &[""], version_args: Some(&["-v"]) },
    ToolDef { id: "composer", name: "Composer", exe: "composer.phar", path_dirs: &[""], version_args: None },
    ToolDef { id: "git", name: "Git", exe: "cmd/git.exe", path_dirs: &["cmd"], version_args: Some(&["--version"]) },
    ToolDef { id: "python", name: "Python", exe: "python.exe", path_dirs: &["", "Scripts"], version_args: Some(&["--version"]) },
];

pub fn find(id: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.id == id)
}

/// One installed copy of a tool.
pub struct Install {
    /// Directory name shown in the UI; "default" for flat installs (e.g. `bin/git/cmd/git.exe`).
    pub label: String,
    pub dir: PathBuf,
}

impl ToolDef {
    pub fn tool_dir(&self, bin: &Path) -> PathBuf {
        bin.join(self.id)
    }

    /// Installed versions, newest first. A tool may be "flat" (exe directly in `bin/<id>/`)
    /// or versioned (`bin/<id>/<version-dir>/`).
    pub fn installs(&self, bin: &Path) -> Vec<Install> {
        let tool_dir = self.tool_dir(bin);
        if tool_dir.join(self.exe).is_file() {
            return vec![Install { label: "default".into(), dir: tool_dir }];
        }

        let mut found: Vec<Install> = std::fs::read_dir(&tool_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().join(self.exe).is_file())
            .map(|e| Install { label: e.file_name().to_string_lossy().into_owned(), dir: e.path() })
            .collect();
        found.sort_by(|a, b| natural_cmp(&b.label, &a.label));
        found
    }

    pub fn path_dirs_for(&self, install_dir: &Path) -> Vec<PathBuf> {
        self.path_dirs
            .iter()
            .map(|d| if d.is_empty() { install_dir.to_path_buf() } else { install_dir.join(d) })
            .collect()
    }

    /// First line of the tool's own version output, e.g. "PHP 8.3.33 (cli) ...".
    pub fn read_version(&self, install_dir: &Path) -> Option<String> {
        let args = self.version_args?;
        let out = Command::new(install_dir.join(self.exe))
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        // Some tools (older Python) print the version on stderr.
        let text = if out.stdout.is_empty() { out.stderr } else { out.stdout };
        let line = String::from_utf8_lossy(&text).lines().next()?.trim().to_string();
        // mysqld prefixes its own full path: "D:\...\mysqld.exe  Ver 8.4.3 ..." -> "Ver 8.4.3 ..."
        let line = match line.split_once(char::is_whitespace) {
            Some((first, rest)) if first.contains(['\\', '/']) => rest.trim().to_string(),
            _ => line,
        };
        Some(line).filter(|l| !l.is_empty())
    }
}

/// Compares strings treating digit runs as numbers, so "php-8.10" > "php-8.9".
pub(crate) fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek(), b.peek()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut n: u64 = 0;
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        n = n.saturating_mul(10).saturating_add(c as u64 - '0' as u64);
                        it.next();
                    }
                    n
                };
                match take(&mut a).cmp(&take(&mut b)) {
                    Ordering::Equal => continue,
                    o => return o,
                }
            }
            (Some(x), Some(y)) => match x.to_ascii_lowercase().cmp(&y.to_ascii_lowercase()) {
                Ordering::Equal => {
                    a.next();
                    b.next();
                }
                o => return o,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::natural_cmp;
    use std::cmp::Ordering;

    #[test]
    fn natural_ordering() {
        assert_eq!(natural_cmp("php-8.10.0", "php-8.9.1"), Ordering::Greater);
        assert_eq!(natural_cmp("node-v22", "node-v20"), Ordering::Greater);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
    }

    /// Lists what is detected in the real `bin/`: `cargo test detect_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn detect_real() {
        let bin = crate::paths::bin_dir();
        for tool in super::TOOLS {
            for install in tool.installs(&bin) {
                println!("{:<11} {:<40} {:?}", tool.id, install.label, tool.read_version(&install.dir));
            }
        }
    }
}
