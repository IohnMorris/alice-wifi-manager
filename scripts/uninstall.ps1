param(
    [Parameter(Mandatory = $true)]
    [string]$HostName,
    [string]$User = "root"
)

$ErrorActionPreference = "Stop"
ssh "$User@$HostName" "set -eu; mount -o remount,rw / 2>/dev/null || true; if [ -x /usr/bin/alice-wifi-manager ]; then /usr/bin/alice-wifi-manager uninstall --yes; else /etc/init.d/S99z-wifi-manager stop 2>/dev/null || true; fi"
Write-Host "Manager removed; recovery copy and Wi-Fi configuration were preserved."
