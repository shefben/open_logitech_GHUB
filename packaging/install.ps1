<#
.SYNOPSIS
    Installs ghub-rs for the current user.

.DESCRIPTION
    A per-user install, by design and for the same reason the autostart entry is per-user: this
    application manages one person's peripherals, it never elevates, and nothing it does needs
    administrator rights. So:

      * the files go to %LOCALAPPDATA%\Programs\ghub-rs,
      * the Start-menu shortcut goes to this account's Start menu,
      * the uninstall entry goes under HKCU, so "Installed apps" lists it for this user only.

    What it deliberately does not do:

      * install a service or a scheduled task (both are routes to running elevated) - the
        resident agent is an ordinary user process started from this account's Run key,
      * install a driver or a filter (nothing here needs one),
      * touch HKEY_LOCAL_MACHINE or Program Files,
      * enable "start with Windows" - that is a tick in the application's own Options page, so
        the choice stays with the user rather than being made by an installer.

    Profiles live in %APPDATA%\ghub-rs and are never written or removed by this script. An
    install over an existing one keeps them.

.PARAMETER Source
    The staged directory to install from. Defaults to the directory this script is in, which is
    where build-release.ps1 puts it.

.PARAMETER NoShortcut
    Skips the Start-menu shortcut.
#>
[CmdletBinding()]
param(
    [string] $Source = $PSScriptRoot,
    [switch] $NoShortcut
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$appName = 'ghub-rs'
$displayName = 'G HUB (ghub-rs)'
$target = Join-Path $env:LOCALAPPDATA "Programs\$appName"
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appName"

# Both halves, and both are required: the window finds the agent beside itself, and the agent
# finds the window the same way. Installing one without the other gives a window that silently
# stops managing the devices when it is closed.
$binaries = @('ghub.exe', 'ghub-agent.exe')
foreach ($name in $binaries) {
    if (-not (Test-Path (Join-Path $Source $name))) {
        throw "$name is not in $Source. Run packaging\build-release.ps1 first."
    }
}
if (-not (Test-Path (Join-Path $Source 'assets'))) {
    throw "assets\ is not in $Source. The application would start with no icons."
}

# A running copy holds its own exe open, and the single-instance mutex means the new one would
# refuse to start anyway. Tell the user rather than failing on a locked file. The agent is the one
# that is normally running, because it is the one that stays.
$running = Get-Process -Name 'ghub', 'ghub-agent' -ErrorAction SilentlyContinue
if ($running) {
    $names = ($running | Select-Object -ExpandProperty Name -Unique) -join ', '
    throw "$names is running. Quit it from the notification area (right-click the G icon, Quit) and run this again."
}

Write-Host "Installing $displayName to $target"
New-Item -ItemType Directory -Force -Path $target | Out-Null

# The assets directory is replaced rather than merged: a renamed icon left behind from an older
# version would be dead weight, and a removed one would still be found by name.
$assetTarget = Join-Path $target 'assets'
if (Test-Path $assetTarget) {
    Remove-Item -Recurse -Force $assetTarget
}

foreach ($name in $binaries) {
    Copy-Item (Join-Path $Source $name) (Join-Path $target $name) -Force
}
Copy-Item (Join-Path $Source 'assets') $assetTarget -Recurse -Force
foreach ($extra in @('uninstall.ps1', 'README.txt')) {
    $path = Join-Path $Source $extra
    if (Test-Path $path) {
        Copy-Item $path $target -Force
    }
}

$exe = Join-Path $target 'ghub.exe'
$version = (Get-Item $exe).VersionInfo.FileVersion
if ([string]::IsNullOrWhiteSpace($version)) {
    # The binary carries no version resource, so the one in Cargo.toml is used when it is
    # reachable and the field is left honest rather than invented when it is not.
    $cargo = Join-Path (Split-Path -Parent $Source) 'Cargo.toml'
    if (Test-Path $cargo) {
        $match = Select-String -Path $cargo -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
        if ($match) {
            $version = $match.Matches[0].Groups[1].Value
        }
    }
}

if (-not $NoShortcut) {
    $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
    $shortcut = Join-Path $startMenu "$displayName.lnk"
    $shell = New-Object -ComObject WScript.Shell
    $link = $shell.CreateShortcut($shortcut)
    $link.TargetPath = $exe
    $link.WorkingDirectory = $target
    $link.Description = 'Peripheral manager for Logitech G devices'
    $link.Save()
    Write-Host "  Start-menu shortcut: $shortcut"
}

$size = [int](((Get-ChildItem -Recurse -File $target | Measure-Object -Property Length -Sum).Sum) / 1KB)
New-Item -Path $uninstallKey -Force | Out-Null
$values = @{
    DisplayName     = $displayName
    InstallLocation = $target
    DisplayIcon     = $exe
    Publisher       = 'ghub-rs'
    NoModify        = 1
    NoRepair        = 1
    EstimatedSize   = $size
    UninstallString = "powershell.exe -NoProfile -ExecutionPolicy Bypass -File `"$(Join-Path $target 'uninstall.ps1')`""
}
if (-not [string]::IsNullOrWhiteSpace($version)) {
    $values['DisplayVersion'] = $version
}
foreach ($name in $values.Keys) {
    # `$(...)` rather than a ternary: this script has to run under Windows PowerShell 5.1, which
    # is what a fresh Windows install has and what a double-clicked .ps1 uses.
    $kind = $(if ($values[$name] -is [int]) { 'DWord' } else { 'String' })
    New-ItemProperty -Path $uninstallKey -Name $name -Value $values[$name] -PropertyType $kind -Force | Out-Null
}

Write-Host "  Files:   $target"
Write-Host "  Listed in Installed apps for this user"
Write-Host ''
Write-Host 'Done. Start it from the Start menu.'
Write-Host 'Profiles are kept in %APPDATA%\ghub-rs and are untouched by install and uninstall.'
Write-Host '"Start with Windows" is a setting in the application''s Options page, not something this installer turns on.'
