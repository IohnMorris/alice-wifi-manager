param(
    [Parameter(Mandatory = $true)]
    [string]$Serial
)

$ErrorActionPreference = "Stop"
$adb = Get-Command adb -ErrorAction SilentlyContinue
if (-not $adb) { throw "adb was not found in PATH. Install Android platform-tools first." }

$root = Split-Path -Parent $PSScriptRoot
$target = "armv7-unknown-linux-musleabihf"
$files = @{
    manager = Join-Path $root "target\$target\release\alice-wifi-manager"
    wpa = Join-Path $root "artifacts\armv7\wpa_supplicant"
    init = Join-Path $root "scripts\S99z-wifi-manager"
    transaction = Join-Path $root "scripts\transaction-install.sh"
}
foreach ($file in $files.Values) {
    if (-not (Test-Path $file -PathType Leaf)) { throw "Required package file is missing: $file" }
}

& $adb.Source -s $Serial get-state
if ($LASTEXITCODE -ne 0) { throw "ADB device is not ready." }

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$remote = "/tmp/alice-wifi-manager-$stamp-$PID"
& $adb.Source -s $Serial shell "umask 077; mkdir '$remote'"
if ($LASTEXITCODE -ne 0) { throw "Unable to create ADB staging directory." }
foreach ($entry in $files.GetEnumerator()) {
    & $adb.Source -s $Serial push $entry.Value "$remote/$($entry.Key -replace 'manager','alice-wifi-manager' -replace 'init','S99z-wifi-manager' -replace 'transaction','transaction-install.sh')"
    if ($LASTEXITCODE -ne 0) { throw "ADB upload failed for $($entry.Key)." }
}

& $adb.Source -s $Serial shell "sh '$remote/transaction-install.sh' '$remote' '$stamp-$PID'"
if ($LASTEXITCODE -ne 0) { throw "Transactional ADB installation failed; recovery was retained." }
& $adb.Source -s $Serial shell "/usr/bin/alice-wifi-manager status"
if ($LASTEXITCODE -ne 0) { throw "Manager status check failed." }
Write-Host "ADB deployment completed. Recovery: /bkupgrade/data/recovery-$stamp-$PID"
