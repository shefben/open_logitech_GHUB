<#
.SYNOPSIS
    Builds the ghub-rs MSI installer from the staged release in dist\.

.DESCRIPTION
    1. Runs packaging\build-release.ps1 (cargo build --release + stage dist\) unless -SkipBuild.
    2. Regenerates ghub.ico from assets\icons\g-logo.png if it is missing.
    3. Builds packaging\msi\ghub-rs.wxs with WiX v6 into dist\ghub-rs-<version>-x64.msi.

    Requires the WiX .NET tool and its UI/Util extensions:
        dotnet tool install --global wix --version 6.0.2
        wix extension add -g WixToolset.UI.wixext/6.0.2
        wix extension add -g WixToolset.Util.wixext/6.0.2

.PARAMETER SkipBuild
    Use dist\ as it is instead of rebuilding it first.

.PARAMETER Wix
    Path to wix.exe, if it is not on PATH.
#>
[CmdletBinding()]
param(
    [switch] $SkipBuild,
    [string] $Wix
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$here = $PSScriptRoot
$packaging = Split-Path -Parent $here
$root = Split-Path -Parent $packaging
$dist = Join-Path $root 'dist'

if (-not $Wix) {
    $command = Get-Command wix -ErrorAction SilentlyContinue
    if (-not $command) {
        throw 'wix.exe not found. Install it with: dotnet tool install --global wix --version 6.0.2 (or pass -Wix <path>)'
    }
    $Wix = $command.Source
}

if (-not $SkipBuild) {
    & (Join-Path $packaging 'build-release.ps1') -Clean
}

foreach ($required in @('ghub.exe', 'ghub-agent.exe', 'README.txt', 'assets')) {
    if (-not (Test-Path (Join-Path $dist $required))) {
        throw "$required is not in $dist. Run without -SkipBuild."
    }
}

# Version from the workspace manifest; MSI wants major.minor.build, so pre-release/build
# suffixes (1.2.3-beta+abc) are dropped.
$match = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^\s*version\s*=\s*"([^"]+)"' | Select-Object -First 1
if (-not $match) { throw 'No version found in Cargo.toml' }
$version = ($match.Matches[0].Groups[1].Value -split '[-+]')[0]

$icon = Join-Path $here 'ghub.ico'
if (-not (Test-Path $icon)) {
    & (Join-Path $here 'make-icon.ps1') -Output $icon
}

$msi = Join-Path $dist "ghub-rs-$version-x64.msi"
Write-Host "Building $msi"
Push-Location $here
try {
    & $Wix build ghub-rs.wxs `
        -arch x64 `
        -ext WixToolset.UI.wixext `
        -ext WixToolset.Util.wixext `
        -d "Version=$version" `
        -d "DistDir=$dist" `
        -o $msi
    if ($LASTEXITCODE -ne 0) { throw "wix build failed with exit code $LASTEXITCODE" }
}
finally {
    Pop-Location
}
# WiX leaves a .wixpdb (debug symbols for the package) beside the MSI; not part of a release.
Remove-Item ([System.IO.Path]::ChangeExtension($msi, '.wixpdb')) -ErrorAction SilentlyContinue

$size = (Get-Item $msi).Length
Write-Host ("Built {0} ({1:N0} bytes)" -f $msi, $size)
Write-Host ''
Write-Host 'Install:   double-click it, or  msiexec /i "<msi>"'
Write-Host 'Silent:    msiexec /i "<msi>" /qn AUTOSTART=1 DESKTOPSHORTCUT=0'
Write-Host 'Uninstall: Settings > Apps, or  msiexec /x "<msi>"'
