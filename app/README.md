# Stackyard

Portable local dev stack manager for Windows x64 (Tauri 2 + TypeScript).

## Layout

```
D:\SERVER\            <- app root (in release: the folder containing stackyard.exe)
  bin\<tool>\<version>\   tools: apache, php, mysql, postgresql, nodejs, composer, git, python
  config\                 settings.json (ports, versions), backups\path-*.txt
    apache\ php\ mysql\    generated httpd.conf / php.ini / my.ini + conf.d\ for your overrides
  www\                    Apache document root
  data\mysql-8.4\         MySQL data (created on first start, root has no password)
  logs\  tmp\
  app\                    this source code
```

Configs are regenerated on every start from `src-tauri\templates\`, so the folder can be moved.
Put your own settings in `config\<service>\conf.d\` (`*.conf`, `*.ini`, `*.cnf`).
php.ini is also written into the PHP folder for the CLI; the original is kept as `php.ini.orig`.

"Open terminal" starts cmd, PowerShell, Git Bash or a program of your choice (e.g. Cmder) in `www\`, with
the selected tool versions first in PATH, optionally as a Windows Terminal tab. The startup scripts are
regenerated in `config\terminal\` each time.

## Development

Requires Rust (rustup), MSVC Build Tools (C++ workload) and Node.

```
npm install
npm run tauri dev      # debug build uses D:\SERVER (two levels up) as the app root
npm run tauri build    # -> src-tauri\target\release\stackyard.exe
npm run release        # -> ..\release\Stackyard-<version>-win-x64.zip with the selected tool versions
                       #    (-- -AllVersions, -- -NoBin, -- -KeepFolder); see scripts\release.ps1
```

Override the root with the `STACKYARD_ROOT` environment variable.

Tests: `cd src-tauri && cargo test`. Ignored tests working on the real `bin\`:
`cargo test detect_real -- --ignored --nocapture` (lists tools) and
`cargo test start_stop_real -- --ignored --nocapture` (starts/stops Apache + MySQL, uses ports from settings.json).

## Translations

UI strings live in `src/i18n/en.ts`. Add a locale by creating `src/i18n/<code>.ts` with the same keys and registering it in `src/i18n/index.ts`.
