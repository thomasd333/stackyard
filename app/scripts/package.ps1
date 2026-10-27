# Builds Stackyard.exe and packs a portable zip into app\release\.
#   npm run package              -> exe + empty bin\ (tools are copied in by the user)
#   npm run package -- -WithBin  -> also includes the tools from ..\bin
param([switch]$WithBin)

$ErrorActionPreference = 'Stop'
$app = Split-Path $PSScriptRoot -Parent
$root = Split-Path $app -Parent
$version = (Get-Content "$app\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version

Push-Location $app
try {
    npm run tauri build
    if ($LASTEXITCODE -ne 0) { throw "Build failed" }
} finally {
    Pop-Location
}

$name = "Stackyard-$version-win-x64"
$out = Join-Path $app "release\$name"
if (Test-Path $out) { Remove-Item -Recurse -Force $out }
New-Item -ItemType Directory -Force "$out\bin", "$out\www" | Out-Null

Copy-Item "$app\src-tauri\target\release\stackyard.exe" "$out\Stackyard.exe"
Copy-Item "$app\src-tauri\templates\index.php" "$out\www\index.php"

if ($WithBin) {
    Copy-Item "$root\bin\*" "$out\bin" -Recurse
} else {
    @"
Put tools here, one folder per tool, optionally with version subfolders:

  bin\apache\httpd-2.4.x-Win64-VS18\     (Apache Lounge build)
  bin\php\php-8.x.x-Win32-vs16-x64\      (Thread Safe build, needed by Apache)
  bin\mysql\mysql-8.x.x-winx64\
  bin\postgresql\<version>\
  bin\nodejs\<version>\
  bin\composer\composer.phar
  bin\git\                               (PortableGit)
  bin\python\<version>\
"@ | Set-Content "$out\bin\README.txt" -Encoding ascii
}

$zip = "$out.zip"
if (Test-Path $zip) { Remove-Item $zip }
Compress-Archive -Path "$out\*" -DestinationPath $zip
Write-Host "Created $zip ($([math]::Round((Get-Item $zip).Length / 1MB, 1)) MB)"
