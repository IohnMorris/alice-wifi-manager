param(
    [Parameter(Mandatory = $true)]
    [string]$SpeakerIp,
    [string]$User = "root"
)

$ErrorActionPreference = "Stop"

Write-Host "[1/4] Building static binary for armv7-unknown-linux-musleabihf..." -ForegroundColor Yellow
cargo build --release --target armv7-unknown-linux-musleabihf

$BinPath = "target/armv7-unknown-linux-musleabihf/release/alice-wifi-manager"
$InitPath = "scripts/S99z-wifi-manager"

if (-not (Test-Path $BinPath)) {
    throw "Binary not found at $BinPath"
}

Write-Host "[2/4] Uploading binary and init script to ${User}@${SpeakerIp}..." -ForegroundColor Yellow
scp $BinPath "${User}@${SpeakerIp}:/tmp/alice-wifi-manager"
scp $InitPath "${User}@${SpeakerIp}:/tmp/S99z-wifi-manager"

Write-Host "[3/4] Installing files and setting permissions..." -ForegroundColor Yellow
$RemoteCmd = "mount -o remount,rw / 2>/dev/null || true; mv /tmp/alice-wifi-manager /usr/bin/alice-wifi-manager; chmod 0755 /usr/bin/alice-wifi-manager; mv /tmp/S99z-wifi-manager /etc/init.d/S99z-wifi-manager; chmod 0755 /etc/init.d/S99z-wifi-manager; sed -i 's/\r$//' /etc/init.d/S99z-wifi-manager; /etc/init.d/S99z-wifi-manager restart"
ssh "${User}@${SpeakerIp}" $RemoteCmd

Write-Host "[4/4] Verifying service status..." -ForegroundColor Green
ssh "${User}@${SpeakerIp}" "/usr/bin/alice-wifi-manager status"