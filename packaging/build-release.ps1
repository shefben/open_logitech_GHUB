<#
.SYNOPSIS
    Builds the release binary and stages everything the application needs to run into dist/.

.DESCRIPTION
    Two executables, because the application is two processes. ghub-agent.exe is the resident
    half: it owns the devices, the input hooks, the profile rules, the macros, the lighting and
    the notification-area icon, and it links no GUI toolkit - 1.6 MB against the window's 9.2 MB,
    and ~7 MB of working set against the window's ~390 MB. ghub.exe is the window; it borrows the
    hardware from the agent while it is open and exits completely when closed. Both have to be
    installed: the window starts the agent if it is not running, and finds it *beside itself*.

    Neither is self-contained: they load the device renders, the extracted G HUB icons and the
    pre-decoded tray icon from an `assets` directory beside themselves (see `icons.rs::asset_root`,
    `artwork.rs` and the agent's `icon.rs`). Staging them together is therefore part of building a
    release, not an extra packaging step - a bare ghub.exe runs but draws no icons.

    Produces:
        dist/ghub.exe
        dist/ghub-agent.exe
        dist/assets/...
        dist/install.ps1
        dist/uninstall.ps1
        dist/README.txt

.PARAMETER Clean
    Removes dist/ first, so a renamed or deleted asset cannot survive in the output.
#>
[CmdletBinding()]
param(
    [switch] $Clean
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'

if ($Clean -and (Test-Path $dist)) {
    Write-Host "Removing $dist"
    Remove-Item -Recurse -Force $dist
}

Write-Host 'Building release...'
Push-Location $root
try {
    # Not --all-targets: tests and benches are not part of a release and building them here would
    # only slow the build down. Both binaries by name rather than --workspace, so a crate added
    # later cannot silently become part of a release.
    & cargo build --release -p ghub-ui -p ghub-agent
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build --release failed with exit code $LASTEXITCODE"
    }
}
finally {
    Pop-Location
}

New-Item -ItemType Directory -Force -Path $dist | Out-Null
foreach ($name in @('ghub.exe', 'ghub-agent.exe')) {
    $built = Join-Path $root "target/release/$name"
    if (-not (Test-Path $built)) {
        throw "The build reported success but $built is not there"
    }
    Copy-Item $built (Join-Path $dist $name) -Force
}

$assets = Join-Path $root 'assets'
if (-not (Test-Path $assets)) {
    throw "assets/ is missing; the application would run with no icons and no device renders"
}
# Removed first, not copied over. `Copy-Item -Recurse` onto an existing directory copies the
# source *into* it - staging twice without this produces dist/assets/assets and a dist twice the
# size it should be. It also means a renamed or deleted asset cannot survive from an older stage.
$assetTarget = Join-Path $dist 'assets'
if (Test-Path $assetTarget) {
    Remove-Item -Recurse -Force $assetTarget
}
Copy-Item $assets $assetTarget -Recurse -Force

Copy-Item (Join-Path $PSScriptRoot 'install.ps1') $dist -Force
Copy-Item (Join-Path $PSScriptRoot 'uninstall.ps1') $dist -Force
Copy-Item (Join-Path $PSScriptRoot 'README.txt') $dist -Force

$size = (Get-ChildItem -Recurse -File $dist | Measure-Object -Property Length -Sum).Sum
Write-Host ''
Write-Host ("Staged {0}" -f $dist)
foreach ($name in @('ghub.exe', 'ghub-agent.exe')) {
    Write-Host ("  {0,-16} {1,12:N0} bytes" -f $name, (Get-Item (Join-Path $dist $name)).Length)
}
Write-Host ("  {0,-16} {1,12:N0} bytes" -f 'total', $size)
Write-Host ''
Write-Host 'Run dist/install.ps1 to install for the current user, or just run dist/ghub.exe -'
Write-Host 'the window starts the agent beside it on its own.'
