param(
    [Parameter(Mandatory = $true)]
    [string]$HostName,
    [string]$User = "root",
    [string]$PackageDir = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($PackageDir)) { $PackageDir = $root }

$files = @{
    manager = Join-Path $PackageDir "alice-wifi-manager"
    wpa = Join-Path $PackageDir "wpa_supplicant"
    init = Join-Path $PackageDir "S99z-wifi-manager"
    transaction = Join-Path $PackageDir "transaction-install.sh"
}
if (-not (Test-Path $files.transaction -PathType Leaf)) {
    $files.transaction = Join-Path $PackageDir "scripts\transaction-install.sh"
}
foreach ($file in $files.Values) {
    if (-not (Test-Path $file -PathType Leaf)) { throw "Missing package file: $file" }
    if ((Get-Item $file).Length -le 0) { throw "Empty package file: $file" }
}

$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$stamp = "$stamp-$PID"
$remote = "/tmp/alice-wifi-manager-$stamp"
$target = "$User@$HostName"

& ssh $target "umask 077; mkdir '$remote'"
if ($LASTEXITCODE -ne 0) { throw "Unable to create remote staging directory" }
# Mini 2 has SSH but no sftp-server; force legacy SCP protocol.
& scp -O $files.manager $files.wpa $files.init $files.transaction "$target`:$remote/"
if ($LASTEXITCODE -ne 0) { throw "Package upload failed" }
& ssh $target "sh '$remote/transaction-install.sh' '$remote' '$stamp'"
if ($LASTEXITCODE -ne 0) { throw "Transactional installation failed; recovery was retained on the device" }
Write-Host "Installation completed. Recovery: /bkupgrade/data/recovery-$stamp"
