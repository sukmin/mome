# Generates assets\icon.png (256 px, window icon) and assets\icon.ico (exe icon): a cat face.
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File scripts\make-icon.ps1
Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent $PSScriptRoot
$assets = Join-Path $root 'assets'
New-Item -ItemType Directory -Force $assets | Out-Null

function New-RoundedPath([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $r * 2
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Pt([float]$x, [float]$y, [float]$s) { New-Object System.Drawing.PointF(($x * $s), ($y * $s)) }

function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = 'AntiAlias'
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0
    $detail = $size -ge 48

    # Background tile.
    $tile = New-RoundedPath (8 * $s) (8 * $s) (240 * $s) (240 * $s) (52 * $s)
    $rect = New-Object System.Drawing.RectangleF(0, 0, $size, $size)
    $top = [System.Drawing.Color]::FromArgb(255, 110, 148, 255)
    $bottom = [System.Drawing.Color]::FromArgb(255, 52, 88, 214)
    $bg = New-Object System.Drawing.Drawing2D.LinearGradientBrush($rect, $top, $bottom, 90)
    $g.FillPath($bg, $tile)

    $white = [System.Drawing.Brushes]::White
    $ink = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 52, 36, 28))
    $pink = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 255, 150, 160))
    # Cheese (orange tabby) fur: light on top, deeper towards the chin.
    $furRect = New-Object System.Drawing.RectangleF(0, (30 * $s), $size, (205 * $s))
    $fur = New-Object System.Drawing.Drawing2D.LinearGradientBrush($furRect,
        [System.Drawing.Color]::FromArgb(255, 255, 196, 112), [System.Drawing.Color]::FromArgb(255, 240, 146, 52), 90)
    $stripe = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 210, 106, 26))
    $cream = New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 255, 244, 228))

    # Ears, then the head on top of their bases.
    $g.FillPolygon($fur, [System.Drawing.PointF[]]@((Pt 52 140 $s), (Pt 66 34 $s), (Pt 126 98 $s)))
    $g.FillPolygon($fur, [System.Drawing.PointF[]]@((Pt 204 140 $s), (Pt 190 34 $s), (Pt 130 98 $s)))
    $g.FillEllipse($fur, 40 * $s, 84 * $s, 176 * $s, 148 * $s)
    if ($detail) {
        $g.FillPolygon($pink, [System.Drawing.PointF[]]@((Pt 76 104 $s), (Pt 80 62 $s), (Pt 108 94 $s)))
        $g.FillPolygon($pink, [System.Drawing.PointF[]]@((Pt 180 104 $s), (Pt 176 62 $s), (Pt 148 94 $s)))
    }

    # Tabby stripes: two on each cheek.
    if ($detail) {
        foreach ($y in 150, 166) {
            $g.FillPath($stripe, (New-RoundedPath (50 * $s) ($y * $s) (22 * $s) (8 * $s) (4 * $s)))
            $g.FillPath($stripe, (New-RoundedPath (184 * $s) ($y * $s) (22 * $s) (8 * $s) (4 * $s)))
        }
    }

    # Cream muzzle.
    if ($detail) {
        $g.FillEllipse($cream, 92 * $s, 164 * $s, 72 * $s, 54 * $s)
    }

    # Eyes.
    if ($detail) {
        $g.FillEllipse($ink, 82 * $s, 136 * $s, 24 * $s, 32 * $s)
        $g.FillEllipse($ink, 150 * $s, 136 * $s, 24 * $s, 32 * $s)
        $g.FillEllipse($white, 92 * $s, 142 * $s, 8 * $s, 9 * $s)
        $g.FillEllipse($white, 160 * $s, 142 * $s, 8 * $s, 9 * $s)
    } else {
        $g.FillEllipse($ink, 78 * $s, 132 * $s, 34 * $s, 40 * $s)
        $g.FillEllipse($ink, 144 * $s, 132 * $s, 34 * $s, 40 * $s)
    }

    if ($detail) {
        # Nose and mouth.
        $g.FillPolygon($pink, [System.Drawing.PointF[]]@((Pt 117 178 $s), (Pt 139 178 $s), (Pt 128 191 $s)))
        $mouth = New-Object System.Drawing.Pen($ink.Color, [float](4 * $s))
        $mouth.StartCap = 'Round'; $mouth.EndCap = 'Round'
        $g.DrawArc($mouth, 104 * $s, 182 * $s, 24 * $s, 20 * $s, 20, 140)
        $g.DrawArc($mouth, 128 * $s, 182 * $s, 24 * $s, 20 * $s, 20, 140)
        # Whiskers.
        $whisker = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(170, 255, 255, 255), [float](3.5 * $s))
        $whisker.StartCap = 'Round'; $whisker.EndCap = 'Round'
        $g.DrawLine($whisker, (Pt 70 180 $s), (Pt 22 170 $s))
        $g.DrawLine($whisker, (Pt 70 192 $s), (Pt 24 198 $s))
        $g.DrawLine($whisker, (Pt 186 180 $s), (Pt 234 170 $s))
        $g.DrawLine($whisker, (Pt 186 192 $s), (Pt 232 198 $s))
    }

    $g.Dispose()
    return $bmp
}

$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = @()
foreach ($size in $sizes) {
    $bmp = New-IconBitmap $size
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += , $ms.ToArray()
    if ($size -eq 256) { $bmp.Save((Join-Path $assets 'icon.png'), [System.Drawing.Imaging.ImageFormat]::Png) }
    $bmp.Dispose()
}

# ICO container with PNG-compressed images (supported since Windows Vista).
$out = New-Object System.IO.MemoryStream
$w = New-Object System.IO.BinaryWriter($out)
$w.Write([UInt16]0); $w.Write([UInt16]1); $w.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $w.Write([Byte]$dim); $w.Write([Byte]$dim); $w.Write([Byte]0); $w.Write([Byte]0)
    $w.Write([UInt16]1); $w.Write([UInt16]32)
    $w.Write([UInt32]$pngs[$i].Length); $w.Write([UInt32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($png in $pngs) { $w.Write($png) }
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $assets 'icon.ico'), $out.ToArray())
Write-Host "wrote assets\icon.png and assets\icon.ico"
