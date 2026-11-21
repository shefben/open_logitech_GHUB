<#
.SYNOPSIS
    Removes a per-user ghub-rs install.

.DESCRIPTION
    Removes exactly what install.ps1 created: the install directory, the Start-menu shortcut, the
    uninstall entry, and the "start with Windows" value if it is there.

    Profiles, macros and the automation token are **kept**. They are the user's own data and live
    in %APPDATA%\ghub-rs; -RemoveSettings deletes them, and nothing else does.

    The install directory is checked against the expected path before anything is deleted. The
    script takes no path from the caller for that reason: an uninstaller that deletes whatever
    directory it is pointed at is a tool for deleting directories.

.PARAMETER RemoveSettings
    Also deletes %APPDATA%\ghub-rs: every profile, every macro and the automation API token.
    Irreversible.
#>
[CmdletBinding()]
param(
    [switch] $RemoveSettings
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$appName = 'ghub-rs'
$displayName = 'G HUB (ghub-rs)'
$target = Join-Path $env:LOCALAPPDATA "Programs\$appName"
$settings = Join-Path $env:APPDATA $appName
$uninstallKey = "HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$appName"
$runKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'

$running = Get-Process -Name 'ghub', 'ghub-agent' -ErrorAction SilentlyContinue
if ($running) {
    throw 'ghub.exe is running. Quit it from the notification area (right-click the G icon, Quit) and run this again.'
}

# The startup entry first: an entry pointing at an executable that is no longer there fails at
# every logon with nothing to explain it.
$autostart = Get-ItemProperty -Path $runKey -Name $appName -ErrorAction SilentlyContinue
if ($autostart) {
    Remove-ItemProperty -Path $runKey -Name $appName -Force
    Write-Host '  Removed the "start with Windows" entry'
}

$shortcut = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\$displayName.lnk"
if (Test-Path $shortcut) {
    Remove-Item $shortcut -Force
    Write-Host '  Removed the Start-menu shortcut'
}

if (Test-Path $uninstallKey) {
    Remove-Item $uninstallKey -Recurse -Force
    Write-Host '  Removed the Installed apps entry'
}

if (Test-Path $target) {
    # The guard: only ever this one path, under this account's own Programs directory.
    $resolved = (Resolve-Path $target).Path
    $expected = (Resolve-Path (Join-Path $env:LOCALAPPDATA 'Programs')).Path
    if (-not $resolved.StartsWith($expected, [System.StringComparison]::OrdinalIgnoreCase)) {
        # `${resolved}` delimited: a `$name:` in a double-quoted string is parsed as a
        # drive-qualified variable reference, which is a parse error rather than a wrong message.
        throw "Refusing to delete ${resolved}: it is not under $expected"
    }
    # This script lives inside the directory it is deleting, which Windows permits (the file is
    # already read) but which leaves the directory behind if it is held open. Reported rather
    # than retried in a loop.
    try {
        Remove-Item -Recurse -Force $resolved
        Write-Host "  Removed $resolved"
    }
    catch {
        Write-Warning "Could not remove $resolved ($($_.Exception.Message)). Delete it by hand."
    }
}
else {
    Write-Host "  $target was not there"
}

if ($RemoveSettings) {
    if (Test-Path $settings) {
        Remove-Item -Recurse -Force $settings
        Write-Host "  Removed $settings (profiles, macros and the automation token)"
    }
}
elseif (Test-Path $settings) {
    Write-Host ''
    Write-Host "Profiles and macros are kept in $settings."
    Write-Host 'Run this script with -RemoveSettings to delete them as well.'
}

Write-Host ''
Write-Host 'Uninstalled.'
