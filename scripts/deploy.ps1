param(
    [Parameter(Mandatory = $true)]
    [string]$SpeakerIp,
    [string]$User = "root"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$target = "armv7-unknown-linux-musleabihf"

Write-Host "[1/3] Building ARMv7 manager..." -ForegroundColor Yellow
cargo build --release --target $target
if ($LASTEXITCODE -ne 0) { throw "ARMv7 build failed" }

$package = Join-Path ([System.IO.Path]::GetTempPath()) ("alice-wifi-manager-deploy-" + [guid]::NewGuid())
try {
    New-Item -ItemType Directory -Path $package -Force | Out-Null
    $manager = Join-Path $root "target\$target\release\alice-wifi-manager"
    $wpa = Join-Path $root "artifacts\armv7\wpa_supplicant"
    $init = Join-Path $root "scripts\S99z-wifi-manager"
    $transaction = Join-Path $root "scripts\transaction-install.sh"
    foreach ($file in @($manager, $wpa, $init, $transaction)) {
        if (-not (Test-Path $file -PathType Leaf)) { throw "Required package file is missing: $file" }
    }
    Copy-Item $manager (Join-Path $package "alice-wifi-manager")
    Copy-Item $wpa (Join-Path $package "wpa_supplicant")
    Copy-Item $init (Join-Path $package "S99z-wifi-manager")
    Copy-Item $transaction (Join-Path $package "transaction-install.sh")

    Write-Host "[2/3] Installing with backup and rollback..." -ForegroundColor Yellow
    & (Join-Path $root "scripts\install.ps1") -HostName $SpeakerIp -User $User -PackageDir $package
    if ($LASTEXITCODE -ne 0) { throw "Transactional installation failed" }
    Write-Host "[3/3] Transactional deployment completed." -ForegroundColor Green
}
finally {
    if (Test-Path $package) { Remove-Item -Recurse -Force $package }
}
