param(
    [Parameter(Mandatory = $true)]
    [string]$HostName,
    [string]$User = "root"
)

$ErrorActionPreference = "Stop"
$archive = "alice-wifi-manager-armv7.tar.gz"
$baseUrl = "https://github.com/IohnMorris/alice-wifi-manager/releases/latest/download"
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("alice-wifi-manager-" + [guid]::NewGuid())
$archivePath = Join-Path $work $archive
$sumsPath = Join-Path $work "SHA256SUMS"
$packageRoot = Join-Path $work "package"

try {
    New-Item -ItemType Directory -Path $packageRoot -Force | Out-Null
    Invoke-WebRequest -Uri "$baseUrl/$archive" -OutFile $archivePath
    Invoke-WebRequest -Uri "$baseUrl/SHA256SUMS" -OutFile $sumsPath

    $expected = ((Select-String -Path $sumsPath -Pattern ("  " + [regex]::Escape($archive) + "$")).Line -split "\s+")[0]
    if ([string]::IsNullOrWhiteSpace($expected)) { throw "Archive checksum is missing" }
    $actual = (Get-FileHash -Path $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $expected.ToLowerInvariant()) { throw "Archive checksum mismatch" }

    tar -xzf $archivePath -C $packageRoot
    $package = Join-Path $packageRoot "alice-wifi-manager"
    & (Join-Path $package "scripts\install.ps1") `
        -HostName $HostName -User $User -PackageDir $package
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
}
