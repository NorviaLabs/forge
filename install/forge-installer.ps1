param(
    [string]$Version = $env:FORGE_VERSION,
    [string]$InstallDir = $(if ($env:FORGE_INSTALL_DIR) { $env:FORGE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Forge\bin" })
)

$ErrorActionPreference = "Stop"

$Repository = if ($env:FORGE_REPOSITORY) { $env:FORGE_REPOSITORY } else { "NorviaLabs/forge" }

function Fail([string]$Message) {
    throw "forge installer: $Message"
}

if (-not $Version) {
    $Releases = Invoke-RestMethod -Headers @{ Accept = "application/vnd.github+json" } `
        -Uri "https://api.github.com/repos/$Repository/releases?per_page=20"
    $Version = $Releases[0].tag_name
}
if (-not $Version) { Fail "could not determine the latest release; set FORGE_VERSION" }

$Architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString()
if ($Architecture -ne "X64") { Fail "unsupported architecture: $Architecture" }
$Target = "x86_64-pc-windows-msvc"
$Asset = "forge-$Version-$Target.tar.gz"
$BaseUrl = "https://github.com/$Repository/releases/download/$Version"
$TempDir = Join-Path ([System.IO.Path]::GetTempPath()) ("forge-installer-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $TempDir | Out-Null

try {
    $Archive = Join-Path $TempDir $Asset
    $Checksums = Join-Path $TempDir "SHA256SUMS"
    Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile $Archive
    Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $Checksums

    $Expected = ((Get-Content $Checksums | Where-Object { $_ -match "\s$([regex]::Escape($Asset))$" } | Select-Object -First 1) -split "\s+")[0]
    if (-not $Expected) { Fail "no checksum found for $Asset" }
    $Actual = (Get-FileHash -Algorithm SHA256 -Path $Archive).Hash.ToLowerInvariant()
    if ($Expected.ToLowerInvariant() -ne $Actual) { Fail "checksum verification failed" }

    & tar.exe -xzf $Archive -C $TempDir
    if ($LASTEXITCODE -ne 0) { Fail "could not extract $Asset" }

    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
    Copy-Item (Join-Path $TempDir "forge-$Version-$Target\forge.exe") (Join-Path $InstallDir "forge.exe") -Force

    $UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
    $PathParts = if ($UserPath) { $UserPath -split ";" } else { @() }
    if ($PathParts -notcontains $InstallDir) {
        [Environment]::SetEnvironmentVariable("Path", (($PathParts + $InstallDir) -join ";"), "User")
        Write-Output "Added $InstallDir to your user PATH. Open a new terminal before running forge."
    }
    Write-Output "Installed Forge $Version to $(Join-Path $InstallDir 'forge.exe')"
}
finally {
    if (Test-Path $TempDir) { Remove-Item $TempDir -Recurse -Force }
}
