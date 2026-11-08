param(
    [Parameter(Mandatory = $true)]
    [string]$HostName,
    [string]$User = "root"
)

$ErrorActionPreference = "Stop"
ssh "$User@$HostName" "set -eu; mount -o remount,rw / 2>/dev/null || true; /etc/init.d/S99z-wifi-manager stop 2>/dev/null || true; rm -f /usr/bin/alice-wifi-manager /etc/init.d/S99z-wifi-manager /var/run/alice-wifi-manager.pid /var/run/alice-wifi-manager.sock"
Write-Host "Manager removed; Wi-Fi configuration and wpa_supplicant were preserved."

