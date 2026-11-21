<#
.SYNOPSIS
    Builds ghub.ico (16/24/32/48/64 px, PNG-encoded frames) from assets/icons/g-logo.png.

.DESCRIPTION
    The MSI needs an .ico for Add/Remove Programs and the shortcuts, and the repository only has
    the 64 x 64 logo PNG. Frames larger than the source are not generated: upscaling would only
    produce a blurrier image at the same file cost.
#>
[CmdletBinding()]
param(
    [string] $Source,
    [string] $Output
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Drawing

# Defaults resolved here, not in param(): Windows PowerShell 5.1 leaves $PSScriptRoot empty while
# parameter defaults are evaluated.
if (-not $Source) { $Source = Join-Path (Split-Path -Parent (Split-Path -Parent $PSScriptRoot)) 'assets\icons\g-logo.png' }
if (-not $Output) { $Output = Join-Path $PSScriptRoot 'ghub.ico' }

$sizes = @(16, 24, 32, 48, 64)
$original = [System.Drawing.Bitmap]::FromFile((Resolve-Path $Source).Path)
try {
    $frames = foreach ($size in $sizes) {
        $bitmap = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.DrawImage($original, 0, 0, $size, $size)
        $graphics.Dispose()
        $stream = New-Object System.IO.MemoryStream
        $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
        , $stream.ToArray()
    }
}
finally {
    $original.Dispose()
}

$out = New-Object System.IO.MemoryStream
$writer = New-Object System.IO.BinaryWriter($out)
# ICONDIR: reserved, type 1 (icon), image count.
$writer.Write([uint16]0); $writer.Write([uint16]1); $writer.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $size = $sizes[$i]
    # ICONDIRENTRY. A width or height of 256 is stored as 0; none here reach it.
    $writer.Write([byte]$size); $writer.Write([byte]$size)
    $writer.Write([byte]0); $writer.Write([byte]0)
    $writer.Write([uint16]1); $writer.Write([uint16]32)
    $writer.Write([uint32]$frames[$i].Length); $writer.Write([uint32]$offset)
    $offset += $frames[$i].Length
}
foreach ($frame in $frames) { $writer.Write([byte[]]$frame) }
$writer.Flush()
[System.IO.File]::WriteAllBytes($Output, $out.ToArray())
Write-Host "Wrote $Output ($($out.Length) bytes, $($sizes -join '/') px)"
