# Regenerates src-tauri\icons from the SVG sources in src-tauri\icons\source:
#   icon.svg     full design, used for 48 px and up (and the PNGs / icns Tauri wants)
#   icon-32.svg  simplified for 24 and 32 px
#   icon-16.svg  hand-tuned for 16 px (title bar, tray at 100 % scaling)
# icon.ico gets the small sizes from the simplified sources instead of downscaling the big one,
# and tray.png (32 px, from icon-32.svg) is embedded for the tray icon.
#
#   npm run icons
$ErrorActionPreference = 'Stop'
$app = Split-Path $PSScriptRoot -Parent
$icons = Join-Path $app "src-tauri\icons"
$source = Join-Path $icons "source"
$work = Join-Path (Split-Path $app -Parent) "tmp\icons"

Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $work | Out-Null

Push-Location $app
try {
    npx tauri icon "$source\icon.svg" -o $icons
    if ($LASTEXITCODE -ne 0) { throw "tauri icon failed for icon.svg" }
    npx tauri icon "$source\icon.svg" -o "$work\big" -p 48,64,256
    if ($LASTEXITCODE -ne 0) { throw "tauri icon failed for icon.svg" }
    npx tauri icon "$source\icon-32.svg" -o "$work\s32" -p 24,32
    if ($LASTEXITCODE -ne 0) { throw "tauri icon failed for icon-32.svg" }
    npx tauri icon "$source\icon-16.svg" -o "$work\s16" -p 16
    if ($LASTEXITCODE -ne 0) { throw "tauri icon failed for icon-16.svg" }
} finally {
    Pop-Location
}

# Only Windows is built; the mobile icon sets are not needed.
foreach ($dir in 'android', 'ios') {
    Remove-Item -Recurse -Force (Join-Path $icons $dir) -ErrorAction SilentlyContinue
}

# ICO with PNG entries (supported since Windows Vista): header, one 16-byte entry per image, then the PNGs.
$entries = @(
    @{ Size = 16; Path = "$work\s16\16x16.png" },
    @{ Size = 24; Path = "$work\s32\24x24.png" },
    @{ Size = 32; Path = "$work\s32\32x32.png" },
    @{ Size = 48; Path = "$work\big\48x48.png" },
    @{ Size = 64; Path = "$work\big\64x64.png" },
    @{ Size = 256; Path = "$work\big\256x256.png" }
)
$stream = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter $stream
$writer.Write([UInt16]0)
$writer.Write([UInt16]1)
$writer.Write([UInt16]$entries.Count)
$offset = 6 + 16 * $entries.Count
$data = foreach ($e in $entries) { , [System.IO.File]::ReadAllBytes($e.Path) }
for ($i = 0; $i -lt $entries.Count; $i++) {
    $dim = if ($entries[$i].Size -ge 256) { 0 } else { $entries[$i].Size }
    $writer.Write([Byte]$dim)
    $writer.Write([Byte]$dim)
    $writer.Write([Byte]0)
    $writer.Write([Byte]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]32)
    $writer.Write([UInt32]$data[$i].Length)
    $writer.Write([UInt32]$offset)
    $offset += $data[$i].Length
}
foreach ($bytes in $data) { $writer.Write($bytes) }
$writer.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $icons "icon.ico"), $stream.ToArray())

Copy-Item "$work\s32\32x32.png" (Join-Path $icons "tray.png") -Force
Copy-Item "$work\s32\32x32.png" (Join-Path $icons "32x32.png") -Force
Remove-Item -Recurse -Force $work
Write-Host "Icons written to $icons"
