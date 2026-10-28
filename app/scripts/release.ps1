# Builds a shareable Stackyard zip in <root>\release\.
#
#   npm run release                  exe + the selected version of every tool in bin\
#   npm run release -- -AllVersions  every installed version of every tool
#   npm run release -- -NoBin        exe only, with an empty bin\ and a README
#   npm run release -- -KeepFolder   keep the unpacked folder next to the zip
#
# Nothing from this machine goes in: no www\, data\, config\, logs\, tmp\ or backup\, and the files in
# bin\ that Stackyard, Laragon or the tools themselves wrote (php.ini, my.ini, Apache logs, ...).
# Text files are then scanned for local paths and names; any hit stops the release.
param([switch]$AllVersions, [switch]$NoBin, [switch]$KeepFolder)

$ErrorActionPreference = 'Stop'
$app = Split-Path $PSScriptRoot -Parent
$root = Split-Path $app -Parent
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"

function Step($text) { Write-Host "`n== $text" -ForegroundColor Cyan }

# --- Version: the three manifests must agree -------------------------------------------------------
$version = (Get-Content "$app\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version
$npmVersion = (Get-Content "$app\package.json" -Raw | ConvertFrom-Json).version
$cargoVersion = (Select-String -Path "$app\src-tauri\Cargo.toml" -Pattern '^version\s*=\s*"(.+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if ($version -ne $npmVersion -or $version -ne $cargoVersion) {
    throw "Version mismatch: tauri.conf.json $version, package.json $npmVersion, Cargo.toml $cargoVersion"
}
$name = "Stackyard-$version-win-x64"
Write-Host "Stackyard $version"

$dirty = git -C $root status --porcelain 2>$null
if ($dirty) { Write-Host "Note: the git working tree has uncommitted changes." -ForegroundColor Yellow }

# --- Checks and build -------------------------------------------------------------------------------
Push-Location $app
try {
    Step "Typecheck"
    npx tsc --noEmit
    if ($LASTEXITCODE -ne 0) { throw "TypeScript errors" }

    Step "Tests"
    Push-Location src-tauri
    try {
        cargo test --quiet
        if ($LASTEXITCODE -ne 0) { throw "Tests failed" }
    } finally { Pop-Location }

    Step "Build"
    # Panic messages embed source paths; map local ones (user name, folders) to neutral names. Later
    # matching prefixes win, so the generic profile path goes first. A separate target dir keeps these
    # flags from invalidating the normal `tauri build` cache.
    $env:RUSTFLAGS = @(
        "--remap-path-prefix=$env:USERPROFILE=~",
        "--remap-path-prefix=$env:USERPROFILE\.cargo=cargo",
        "--remap-path-prefix=$env:USERPROFILE\.rustup=rustup",
        "--remap-path-prefix=$root=stackyard"
    ) -join ' '
    $env:CARGO_TARGET_DIR = "$app\src-tauri\target\dist"
    try {
        npm run tauri build
        if ($LASTEXITCODE -ne 0) { throw "Build failed" }
    } finally {
        Remove-Item Env:RUSTFLAGS, Env:CARGO_TARGET_DIR
    }
} finally {
    Pop-Location
}

# --- Stage ------------------------------------------------------------------------------------------
$releaseDir = Join-Path $root "release"
$out = Join-Path $releaseDir $name
$zip = "$out.zip"
foreach ($p in $out, $zip) { if (Test-Path $p) { Remove-Item -Recurse -Force $p } }
New-Item -ItemType Directory -Force "$out\bin", "$out\www" | Out-Null

Copy-Item "$app\src-tauri\target\dist\release\stackyard.exe" "$out\Stackyard.exe"
Copy-Item "$app\src-tauri\templates\index.php" "$out\www\index.php"

# Same detection exes as TOOLS in src-tauri/src/tools.rs.
$tools = [ordered]@{
    apache = 'bin\httpd.exe'; php = 'php.exe'; mysql = 'bin\mysqld.exe'; postgresql = 'bin\postgres.exe'
    nodejs = 'node.exe'; composer = 'composer.phar'; git = 'cmd\git.exe'; python = 'python.exe'
}

# Written on this machine, never shipped. Stackyard regenerates its own configs on start.
$excludeFiles = @(
    'php.ini', 'php.ini.orig',      # generated / Laragon's original with D:/laragon paths
    'my.ini',                       # Laragon's, points at its datadir (Stackyard uses config\mysql\my.ini)
    'httpd.conf',                   # Laragon-edited (Stackyard runs httpd -f config\apache\httpd.conf)
    'access_log', 'error_log', 'httpd.pid', '*.log', '*.pid',
    'composer-*.phar',              # old Composer versions kept after an update
    '*.pdb'                         # debug symbols (~700 MB in the MySQL zip), never needed to run
)
$excludeDirNames = @('__pycache__')
# Debug builds of plugins (MySQL's lib\plugin\debug); relative to each tool or version folder.
$excludeSubDirs = @('logs', 'tmp', 'lib\plugin\debug')

function Get-NaturalKey($s) { [regex]::Replace($s, '\d+', { $args[0].Value.PadLeft(10, '0') }) }

function Copy-Tree($from, $to) {
    $rc = @($from, $to, '/E', '/NFL', '/NDL', '/NJH', '/NJS', '/NP', '/R:1', '/W:1', '/XF') + $excludeFiles +
        @('/XD') + $excludeDirNames + ($excludeSubDirs | ForEach-Object { "$from\$_" })
    robocopy @rc | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "Copying $from failed (robocopy $LASTEXITCODE)" }
    # Apache expects its logs folder to exist.
    if (Test-Path "$from\logs") { New-Item -ItemType Directory -Force "$to\logs" | Out-Null }
}

if ($NoBin) {
    @"
Put tools here, one folder per tool, optionally with version subfolders:

  bin\apache\httpd-2.4.x-Win64-VS18\     (Apache Lounge build)
  bin\php\php-8.x.x-Win32-vs17-x64\      (Thread Safe build, needed by Apache)
  bin\mysql\mysql-8.x.x-winx64\
  bin\nodejs\node-v22.x.x-win-x64\
  bin\composer\composer.phar
  bin\git\                               (PortableGit)
  bin\python\<version>\
"@ | Set-Content "$out\bin\README.txt" -Encoding ascii
} else {
    Step "Copying tools"
    $settingsFile = Join-Path $root "config\settings.json"
    $selected = @{}
    if (Test-Path $settingsFile) {
        $settings = Get-Content $settingsFile -Raw | ConvertFrom-Json
        if ($settings.tools) { $settings.tools.PSObject.Properties | ForEach-Object { $selected[$_.Name] = $_.Value.version } }
    }
    foreach ($id in $tools.Keys) {
        $dir = Join-Path $root "bin\$id"
        if (-not (Test-Path $dir)) { continue }
        if (Test-Path (Join-Path $dir $tools[$id])) {
            Write-Host "  $id"
            Copy-Tree $dir "$out\bin\$id"
            continue
        }
        $versions = @(Get-ChildItem $dir -Directory | Where-Object { Test-Path (Join-Path $_.FullName $tools[$id]) } |
            Sort-Object { Get-NaturalKey $_.Name } -Descending)
        if (-not $versions) { continue }
        if (-not $AllVersions) {
            # The version selected in Stackyard (what is used and tested daily), else the newest.
            $pick = $versions | Where-Object { $_.Name -eq $selected[$id] } | Select-Object -First 1
            $versions = @(if ($pick) { $pick } else { $versions[0] })
        }
        foreach ($v in $versions) {
            Write-Host "  $id\$($v.Name)"
            Copy-Tree $v.FullName "$out\bin\$id\$($v.Name)"
        }
    }

    # Apache, PHP and MySQL need the Visual C++ runtime. App-local copies (which Microsoft allows to
    # redistribute) let the zip run on a PC without the Visual C++ Redistributable. They come from this
    # PC's System32, so keep its redistributable up to date.
    Step "Adding the Visual C++ runtime"
    $runtime = 'vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll'
    $minimum = [version]'14.50'   # Apache Lounge VS18 builds need at least the VS 2026 runtime
    foreach ($f in $runtime) {
        $src = "$env:SystemRoot\System32\$f"
        if (-not (Test-Path $src)) { throw "$f is missing. Install the Visual C++ Redistributable (x64): https://aka.ms/vs/17/release/vc_redist.x64.exe" }
        $v = [version]((Get-Item $src).VersionInfo.FileVersion -replace '[^\d.].*$', '')
        if ($v -lt $minimum) { throw "$f is $v, older than $minimum. Update the Visual C++ Redistributable (x64): https://aka.ms/vs/17/release/vc_redist.x64.exe" }
    }
    Write-Host "  version $v"
    $targets = @(Get-Item "$out\bin\apache\*\bin", "$out\bin\php\*", "$out\bin\mysql\*\bin" -ErrorAction SilentlyContinue)
    foreach ($t in $targets) {
        # A copy the tool ships itself is left alone.
        foreach ($f in $runtime) { if (-not (Test-Path "$t\$f")) { Copy-Item "$env:SystemRoot\System32\$f" $t } }
        Write-Host "  $($t.FullName.Substring($out.Length + 1))"
    }
}

# CRLF so Notepad on older Windows shows the lines.
$readme = (Get-Content "$PSScriptRoot\README-release.txt" -Raw).Replace('{{VERSION}}', $version)
[IO.File]::WriteAllText("$out\README.txt", ($readme -replace "`r?`n", "`r`n"), [Text.Encoding]::ASCII)

# --- Privacy scan -----------------------------------------------------------------------------------
Step "Scanning for local paths and names"
$patterns = @(
    [regex]::Escape($root), [regex]::Escape($root.Replace('\', '/')),
    [regex]::Escape($env:USERPROFILE), [regex]::Escape($env:USERPROFILE.Replace('\', '/')),
    'laragon'
)
$gitName = git config user.name 2>$null
$gitEmail = git config user.email 2>$null
foreach ($x in $gitName, $gitEmail) { if ($x) { $patterns += [regex]::Escape($x) } }
$textExt = '.ini', '.conf', '.cfg', '.json', '.txt', '.bat', '.cmd', '.ps1', '.sh', '.npmrc', '.pth', '.xml', '.yml', '.yaml', '.env'
# Our README explains moving from Laragon, so it is only checked for paths and names.
$readmePath = "$out\README.txt"
$hits = @(Get-ChildItem $out -Recurse -File |
    Where-Object { $_.FullName -ne $readmePath } |
    Where-Object { $textExt -contains $_.Extension.ToLower() -or $_.Name -in 'npmrc', '.npmrc', 'gitconfig', '.gitconfig' } |
    Select-String -Pattern $patterns -List) +
    @(Select-String -Path $readmePath -Pattern ($patterns | Where-Object { $_ -ne 'laragon' }) -List)
if ($hits) {
    $hits | ForEach-Object { Write-Host "  $($_.Path.Substring($out.Length + 1)):$($_.LineNumber): $($_.Line.Trim())" -ForegroundColor Red }
    throw "Local paths or names found in the release (listed above). Exclude those files in scripts\release.ps1."
}
# Our own exe is built here, so it may hold build paths (see RUSTFLAGS above); check it as text too.
$exeText = [Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes("$out\Stackyard.exe"))
foreach ($p in $patterns) {
    if ($exeText -match $p) { throw "Stackyard.exe contains '$($Matches[0])'. Check the --remap-path-prefix flags." }
}
Write-Host "  clean"

# --- Zip --------------------------------------------------------------------------------------------
Step "Packing"
# System32 tar (bsdtar) writes zip with -a; much faster than Compress-Archive for ~1 GB of tools.
& "$env:SystemRoot\System32\tar.exe" -a -c -f $zip -C $releaseDir $name
if ($LASTEXITCODE -ne 0) { throw "Packing failed" }
if (-not $KeepFolder) { Remove-Item -Recurse -Force $out }

$hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
"$hash  $name.zip" | Set-Content "$zip.sha256" -Encoding ascii
Write-Host "`nCreated $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)" -ForegroundColor Green
Write-Host "SHA256 $hash"
