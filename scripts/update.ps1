param(
    [Parameter(Mandatory = $true)]
    [string]$HostName,
    [string]$User = "root",
    [string]$PackageDir = ""
)

$script = Join-Path $PSScriptRoot "install.ps1"
& $script -HostName $HostName -User $User -PackageDir $PackageDir
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

