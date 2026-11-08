param(
    [Parameter(Mandatory = $true)]
    [string]$Serial
)

$ErrorActionPreference = "Stop"

$adb = Get-Command adb -ErrorAction SilentlyContinue
if (-not $adb) {
    throw "adb was not found in PATH. Install Android platform-tools first."
}

$target = "armv7-unknown-linux-musleabihf"
$binary = Join-Path $PSScriptRoot "..\target\$target\release\alice-wifi-manager"
$init = Join-Path $PSScriptRoot "S99z-wifi-manager"

if (-not (Test-Path $binary)) {
    throw "Release binary is missing: $binary. Build it with cargo build --release --target $target."
}

Write-Host "[1/5] Checking ADB device $Serial..." -ForegroundColor Yellow
& $adb.Source -s $Serial get-state
if ($LASTEXITCODE -ne 0) {
    throw "ADB device is not ready."
}

Write-Host "[2/5] Uploading binary and init script..." -ForegroundColor Yellow
& $adb.Source -s $Serial push $binary /tmp/alice-wifi-manager
& $adb.Source -s $Serial push $init /tmp/S99z-wifi-manager
if ($LASTEXITCODE -ne 0) {
    throw "ADB upload failed."
}

Write-Host "[3/5] Installing files..." -ForegroundColor Yellow
$install = @'
mount -o remount,rw / 2>/dev/null || true
mv /tmp/alice-wifi-manager /usr/bin/alice-wifi-manager
chmod 0755 /usr/bin/alice-wifi-manager
mv /tmp/S99z-wifi-manager /etc/init.d/S99z-wifi-manager
chmod 0755 /etc/init.d/S99z-wifi-manager
sed -i 's/\r$//' /etc/init.d/S99z-wifi-manager
'@
& $adb.Source -s $Serial shell $install
if ($LASTEXITCODE -ne 0) {
    throw "Remote installation failed."
}

Write-Host "[4/5] Restarting manager..." -ForegroundColor Yellow
& $adb.Source -s $Serial shell "/etc/init.d/S99z-wifi-manager restart"
if ($LASTEXITCODE -ne 0) {
    throw "Manager restart failed."
}

Write-Host "[5/5] Verifying automatic STA/AP monitor..." -ForegroundColor Green
& $adb.Source -s $Serial shell "/usr/bin/alice-wifi-manager status"
& $adb.Source -s $Serial shell "tail -n 40 /bkupgrade/data/ap_debug.log 2>/dev/null || true"
