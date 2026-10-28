# Tries the newest release zip on a clean Windows: opens Windows Sandbox, unpacks the zip to
# C:\Stackyard inside it and starts Stackyard.exe. Nothing on this machine is touched (the release folder
# is mapped read-only), and everything is gone when the Sandbox window is closed.
#
#   npm run sandbox
#
# Needs the "Windows Sandbox" optional feature (Windows 10/11 Pro or Enterprise).
$ErrorActionPreference = 'Stop'
$app = Split-Path $PSScriptRoot -Parent
$root = Split-Path $app -Parent

$sandboxExe = "$env:SystemRoot\System32\WindowsSandbox.exe"
if (-not (Test-Path $sandboxExe)) {
    throw "Windows Sandbox is not enabled. Turn it on in 'Turn Windows features on or off' (Windows Sandbox), then restart Windows."
}

$zip = Get-ChildItem "$root\release\Stackyard-*-win-x64.zip" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $zip) { throw "No release zip in $root\release. Run npm run release first." }
Write-Host "Testing $($zip.Name)"

$work = Join-Path $root "tmp\sandbox"
New-Item -ItemType Directory -Force $work | Out-Null

# Runs inside the Sandbox at sign-in, as WDAGUtilityAccount.
@"
`$ErrorActionPreference = 'Stop'
`$zip = 'C:\Users\WDAGUtilityAccount\Desktop\release\$($zip.Name)'
tar.exe -xf `$zip -C C:\
Rename-Item 'C:\$($zip.BaseName)' 'C:\Stackyard'
Start-Process explorer.exe 'C:\Stackyard'
Start-Process 'C:\Stackyard\Stackyard.exe'
"@ | Set-Content "$work\setup.ps1" -Encoding ascii

@"
<Configuration>
  <MappedFolders>
    <MappedFolder>
      <HostFolder>$root\release</HostFolder>
      <SandboxFolder>C:\Users\WDAGUtilityAccount\Desktop\release</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
    <MappedFolder>
      <HostFolder>$work</HostFolder>
      <SandboxFolder>C:\Users\WDAGUtilityAccount\Desktop\setup</SandboxFolder>
      <ReadOnly>true</ReadOnly>
    </MappedFolder>
  </MappedFolders>
  <LogonCommand>
    <Command>powershell.exe -NoProfile -ExecutionPolicy Bypass -File C:\Users\WDAGUtilityAccount\Desktop\setup\setup.ps1</Command>
  </LogonCommand>
  <MemoryInMB>4096</MemoryInMB>
</Configuration>
"@ | Set-Content "$work\stackyard.wsb" -Encoding ascii

Start-Process $sandboxExe "`"$work\stackyard.wsb`""
Write-Host "Sandbox is starting; Stackyard opens in it after sign-in (about a minute)."
