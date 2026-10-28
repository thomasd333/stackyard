//! Virtual hosts: `<name>.localhost` sites added by hand, each with its own DocumentRoot.
//!
//! Browsers resolve `*.localhost` to the loopback address on their own, so no hosts-file edit (and no
//! elevation) is needed. Roots under the Stackyard folder are stored relative to it, so moving the
//! folder keeps the sites working.

use std::path::{Path, PathBuf};

use crate::paths::{self, fwd};
use crate::settings::Site;

pub const TLD: &str = "localhost";

/// Checks and normalizes a site name typed by the user ("MyApp.localhost" -> "myapp").
pub fn normalize_name(input: &str) -> Result<String, String> {
    let name = input.trim().to_ascii_lowercase();
    let name = name.strip_suffix(&format!(".{TLD}")).unwrap_or(&name).to_string();
    if name.is_empty() {
        return Err("Enter a site name, e.g. myapp (it becomes myapp.localhost).".into());
    }
    let label_ok = |l: &str| {
        !l.is_empty()
            && l.len() <= 63
            && !l.starts_with('-')
            && !l.ends_with('-')
            && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if name == TLD || !name.split('.').all(label_ok) {
        return Err(format!(
            "\"{}\" is not a valid site name. Use letters a-z, digits and hyphens, e.g. my-app.",
            input.trim()
        ));
    }
    Ok(name)
}

/// Folder of a site; relative roots are relative to the Stackyard folder.
pub fn resolve_root(root: &str) -> PathBuf {
    let path = Path::new(root);
    if path.is_absolute() { path.to_path_buf() } else { paths::app_root().join(path) }
}

/// Checks a folder typed by the user and returns the form to store: relative when it is inside the
/// Stackyard folder, absolute otherwise.
pub fn normalize_root(input: &str) -> Result<String, String> {
    let input = input.trim().trim_matches('"').replace('/', "\\");
    if input.is_empty() {
        return Err("Enter the site folder, e.g. www\\myapp\\public.".into());
    }
    if !input.is_ascii() {
        return Err("The folder path contains non-English characters (such as ą, ó, ż). PHP cannot work \
                    with such paths. Use a folder with a plain name."
            .into());
    }
    let dir = resolve_root(&input);
    if !dir.is_dir() {
        return Err(format!("Folder {} does not exist.", dir.display()));
    }
    let full = |p: PathBuf| std::path::absolute(&p).unwrap_or(p).to_string_lossy().trim_end_matches('\\').to_string();
    let (dir, root) = (full(dir), full(paths::app_root()));
    // `get` is None when the cut falls inside a non-ASCII character of the root.
    let inside = dir.get(..root.len()).is_some_and(|head| head.eq_ignore_ascii_case(&root))
        && dir.as_bytes().get(root.len()) == Some(&b'\\');
    Ok(if inside { dir[root.len() + 1..].to_string() } else { dir })
}

/// Address of a site in the browser; `name` None is the main site (www\).
pub fn url(name: Option<&str>, port: u16) -> String {
    let host = name.map_or_else(|| TLD.to_string(), |n| format!("{n}.{TLD}"));
    if port == 80 { format!("http://{host}/") } else { format!("http://{host}:{port}/") }
}

/// Folders worth suggesting as site roots: `www\<dir>\public` (Laravel and similar) and `www\<dir>`.
pub fn suggestions() -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(paths::www_dir())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort_by_key(|d| d.to_lowercase());
    dirs.iter()
        .flat_map(|d| {
            let public = paths::www_dir().join(d).join("public").is_dir().then(|| format!("www\\{d}\\public"));
            public.into_iter().chain([format!("www\\{d}")])
        })
        .collect()
}

/// Apache virtual hosts for the sites. The first one (localhost -> www\) is the default for any other
/// host name, e.g. 127.0.0.1. Empty without sites, so the config stays as it was before vhosts.
pub fn apache_vhosts(sites: &[Site], port: u16) -> String {
    if sites.is_empty() {
        return String::new();
    }
    let vhost = |server_name: &str, dir: &str| {
        format!(
            "<VirtualHost *:{port}>\n    ServerName {server_name}\n    DocumentRoot \"{dir}\"\n    \
             <Directory \"{dir}\">\n        Options Indexes FollowSymLinks\n        AllowOverride All\n        \
             Require all granted\n    </Directory>\n</VirtualHost>"
        )
    };
    std::iter::once(vhost(TLD, &fwd(&paths::www_dir())))
        .chain(sites.iter().map(|s| vhost(&format!("{}.{TLD}", s.name), &fwd(&resolve_root(&s.root)))))
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(normalize_name(" MyApp ").unwrap(), "myapp");
        assert_eq!(normalize_name("shop.localhost").unwrap(), "shop");
        assert_eq!(normalize_name("api.shop").unwrap(), "api.shop");
        assert_eq!(normalize_name("my-app2").unwrap(), "my-app2");
        for bad in ["", "localhost", "-x", "x-", "a..b", "my_app", "zażółć", "a b"] {
            assert!(normalize_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn urls() {
        assert_eq!(url(Some("shop"), 80), "http://shop.localhost/");
        assert_eq!(url(None, 8088), "http://localhost:8088/");
    }

    #[test]
    fn roots() {
        // The test binary's folder exists and is inside the repo, i.e. under the debug app root.
        let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let stored = normalize_root(&exe_dir.to_string_lossy()).unwrap();
        assert!(Path::new(&stored).is_relative(), "{stored}");
        assert_eq!(resolve_root(&stored), exe_dir);
        assert!(normalize_root("no\\such\\folder").is_err());
        assert!(normalize_root("").is_err());
    }

    #[test]
    fn vhosts() {
        assert_eq!(apache_vhosts(&[], 80), "");
        let conf = apache_vhosts(&[Site { name: "shop".into(), root: "www\\shop\\public".into() }], 8088);
        assert!(conf.starts_with("<VirtualHost *:8088>\n    ServerName localhost\n"));
        assert!(conf.contains("ServerName shop.localhost"));
        assert!(conf.contains("/www/shop/public\""));
    }
}
