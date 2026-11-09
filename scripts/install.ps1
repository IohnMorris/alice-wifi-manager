param(
    [Parameter(Mandatory = $true)]
    [string]$HostName,
    [string]$User = "root",
    [string]$PackageDir = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($PackageDir)) { $PackageDir = $root }

$manager = Join-Path $PackageDir "alice-wifi-manager"
$wpa = Join-Path $PackageDir "wpa_supplicant"
$init = Join-Path $PackageDir "S99z-wifi-manager"
foreach ($file in @($manager, $wpa, $init)) {
    if (-not (Test-Path $file)) { throw "Missing package file: $file" }
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$remote = "/tmp/alice-wifi-manager-$stamp"
ssh "$User@$HostName" "mkdir -p $remote /bkupgrade/data/recovery-$stamp"
scp $manager "$User@$HostName`:$remote/alice-wifi-manager"
scp $wpa "$User@$HostName`:$remote/wpa_supplicant"
scp $init "$User@$HostName`:$remote/S99z-wifi-manager"

$remoteScript = @"
set -eu
mount -o remount,rw / 2>/dev/null || true
cp /usr/bin/alice-wifi-manager /bkupgrade/data/recovery-$stamp/alice-wifi-manager 2>/dev/null || true
cp /usr/sbin/wpa_supplicant /bkupgrade/data/recovery-$stamp/wpa_supplicant 2>/dev/null || true
cp /etc/init.d/S99z-wifi-manager /bkupgrade/data/recovery-$stamp/S99z-wifi-manager 2>/dev/null || true
cp /bkupgrade/data/wpa_supplicant.conf /bkupgrade/data/recovery-$stamp/wpa_supplicant.conf 2>/dev/null || true
cp /bkupgrade/data/wpa_supplicant.conf.bak /bkupgrade/data/recovery-$stamp/wpa_supplicant.conf.bak 2>/dev/null || true
cp /bkupgrade/data/wpa_supplicant.ap.conf /bkupgrade/data/recovery-$stamp/wpa_supplicant.ap.conf 2>/dev/null || true
/etc/init.d/S99z-wifi-manager stop 2>/dev/null || true
install -m 0755 $remote/alice-wifi-manager /usr/bin/alice-wifi-manager
install -m 0755 $remote/wpa_supplicant /usr/sbin/wpa_supplicant
install -m 0755 $remote/S99z-wifi-manager /etc/init.d/S99z-wifi-manager
sed -i 's/\r$//' /etc/init.d/S99z-wifi-manager
 /etc/init.d/S99z-wifi-manager start
count=$(pidof alice-wifi-manager 2>/dev/null | wc -w)
test "$count" -eq 1
/usr/bin/alice-wifi-manager status
rm -rf $remote
"@
ssh "$User@$HostName" $remoteScript
Write-Host "Installation completed. Recovery: /bkupgrade/data/recovery-$stamp"
