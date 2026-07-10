param(
    [string]$Output = "assets/test/phase1-upscale-source.png"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

Add-Type -AssemblyName System.Drawing

$width = 1280
$height = 720
$path = [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $Output))
$directory = [System.IO.Path]::GetDirectoryName($path)
[System.IO.Directory]::CreateDirectory($directory) | Out-Null

$bitmap = [System.Drawing.Bitmap]::new(
    $width,
    $height,
    [System.Drawing.Imaging.PixelFormat]::Format24bppRgb
)
$bitmap.SetResolution(96, 96)
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)

$resources = [System.Collections.Generic.List[System.IDisposable]]::new()
function Keep([System.IDisposable]$Resource) {
    $resources.Add($Resource)
    return $Resource
}

try {
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $graphics.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $graphics.Clear([System.Drawing.Color]::FromArgb(18, 22, 35))

    $background = Keep ([System.Drawing.Drawing2D.LinearGradientBrush]::new(
        [System.Drawing.Rectangle]::new(0, 0, $width, $height),
        [System.Drawing.Color]::FromArgb(35, 48, 78),
        [System.Drawing.Color]::FromArgb(18, 20, 30),
        25.0
    ))
    $graphics.FillRectangle($background, 0, 0, $width, $height)

    # One-pixel grid and diagonal fans expose ringing and loss of fine detail.
    $gridPen = Keep ([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(70, 150, 180, 230), 1.0))
    for ($x = 32; $x -le 416; $x += 8) {
        $graphics.DrawLine($gridPen, $x, 80, $x, 304)
    }
    for ($y = 80; $y -le 304; $y += 8) {
        $graphics.DrawLine($gridPen, 32, $y, 416, $y)
    }

    $whitePen = Keep ([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(235, 245, 255), 1.0))
    $cyanPen = Keep ([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(85, 225, 255), 2.0))
    for ($offset = 0; $offset -le 220; $offset += 11) {
        $graphics.DrawLine($whitePen, 456, 80 + $offset, 680, 304 - $offset)
        $graphics.DrawLine($cyanPen, 704, 80 + $offset, 928, 304 - $offset)
    }

    # Cel-shaded character-like line art gives Anime4K relevant contours.
    $outline = Keep ([System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(12, 16, 27), 7.0))
    $outline.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $skin = Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(255, 214, 184)))
    $hair = Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(52, 66, 118)))
    $shadow = Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(225, 158, 155)))
    $eye = Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(40, 185, 220)))

    $graphics.FillEllipse($skin, 438, 352, 360, 320)
    $graphics.DrawEllipse($outline, 438, 352, 360, 320)
    $graphics.FillPie($hair, 414, 320, 410, 310, 180, 180)
    $graphics.DrawArc($outline, 414, 320, 410, 310, 180, 180)

    $hairPath = Keep ([System.Drawing.Drawing2D.GraphicsPath]::new())
    $hairPath.AddPolygon([System.Drawing.Point[]]@(
        [System.Drawing.Point]::new(448, 435),
        [System.Drawing.Point]::new(490, 330),
        [System.Drawing.Point]::new(535, 440),
        [System.Drawing.Point]::new(594, 328),
        [System.Drawing.Point]::new(640, 444),
        [System.Drawing.Point]::new(710, 338),
        [System.Drawing.Point]::new(780, 458),
        [System.Drawing.Point]::new(790, 370),
        [System.Drawing.Point]::new(438, 350)
    ))
    $graphics.FillPath($hair, $hairPath)
    $graphics.DrawPath($outline, $hairPath)

    $graphics.FillPie($shadow, 438, 352, 360, 320, 25, 78)
    $graphics.DrawArc($outline, 500, 480, 82, 44, 195, 150)
    $graphics.DrawArc($outline, 654, 480, 82, 44, 195, 150)
    $graphics.FillEllipse($eye, 528, 498, 25, 31)
    $graphics.FillEllipse($eye, 681, 498, 25, 31)
    $graphics.DrawArc($outline, 584, 548, 72, 48, 15, 150)

    # Concentric curves and alternating bars expose haloing and edge contrast.
    for ($i = 0; $i -lt 14; $i++) {
        $color = if (($i % 2) -eq 0) {
            [System.Drawing.Color]::FromArgb(250, 90, 150)
        } else {
            [System.Drawing.Color]::FromArgb(80, 220, 255)
        }
        $pen = Keep ([System.Drawing.Pen]::new($color, 1.0 + ($i % 3)))
        $graphics.DrawEllipse($pen, 870 + $i * 8, 352 + $i * 8, 330 - $i * 16, 300 - $i * 16)
    }

    for ($i = 0; $i -lt 28; $i++) {
        $brush = if (($i % 2) -eq 0) {
            Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(238, 242, 250)))
        } else {
            Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(22, 26, 40)))
        }
        $graphics.FillRectangle($brush, 32 + $i * 14, 344, 14, 104)
    }

    $titleFont = Keep ([System.Drawing.Font]::new("Segoe UI", 24, [System.Drawing.FontStyle]::Bold))
    $labelFont = Keep ([System.Drawing.Font]::new("Consolas", 13, [System.Drawing.FontStyle]::Regular))
    $textBrush = Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(242, 246, 255)))
    $mutedBrush = Keep ([System.Drawing.SolidBrush]::new([System.Drawing.Color]::FromArgb(155, 180, 220)))
    $graphics.DrawString("LimeWall 720p Upscale Test", $titleFont, $textBrush, 30, 22)
    $graphics.DrawString("1 px grid", $labelFont, $mutedBrush, 32, 312)
    $graphics.DrawString("diagonal 1 px / 2 px", $labelFont, $mutedBrush, 456, 312)
    $graphics.DrawString("cel contours", $labelFont, $mutedBrush, 520, 680)
    $graphics.DrawString("rings", $labelFont, $mutedBrush, 1015, 665)

    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    Write-Output "Generated $path ($width x $height)"
} finally {
    $graphics.Dispose()
    for ($index = $resources.Count - 1; $index -ge 0; $index--) {
        $resources[$index].Dispose()
    }
    $bitmap.Dispose()
}
