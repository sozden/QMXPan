# Natural Earth (kamu malı) kara poligonlarından eşdikdörtgen kara maskesi üretir.
# Kullanım: .\tools\landmask.ps1 -GeoJson ne_110m_land.geojson -W 520 -H 260
# Çıktı: assets\land_<W>x<H>.bin (satır satır, bayt başına 8 piksel, en yüksek bit solda)
param(
    [Parameter(Mandatory = $true)][string]$GeoJson,
    [int]$W = 520,
    [int]$H = 260,
    [string]$Png = ""
)
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing
$json = Get-Content -Raw $GeoJson | ConvertFrom-Json
$bmp = New-Object System.Drawing.Bitmap $W, $H
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.Clear([System.Drawing.Color]::Black)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::None
$brush = [System.Drawing.Brushes]::White

function Ring($ring) {
    $pts = foreach ($p in $ring) {
        New-Object System.Drawing.PointF ([float](($p[0] + 180.0) / 360.0 * $W)), ([float]((90.0 - $p[1]) / 180.0 * $H))
    }
    return , [System.Drawing.PointF[]]$pts
}

foreach ($f in $json.features) {
    $geo = $f.geometry
    # Polygon tek poligondur; MultiPolygon bir poligon listesi
    $polys = $geo.coordinates
    if ($geo.type -eq "Polygon") { $polys = @(, $geo.coordinates) }
    foreach ($poly in $polys) {
        $path = New-Object System.Drawing.Drawing2D.GraphicsPath
        $path.FillMode = [System.Drawing.Drawing2D.FillMode]::Alternate
        foreach ($ring in $poly) { $path.AddPolygon((Ring $ring)) }
        $g.FillPath($brush, $path)
    }
}
$g.Dispose()

$stride = [int][Math]::Ceiling($W / 8)
$out = New-Object byte[] ($stride * $H)
for ($y = 0; $y -lt $H; $y++) {
    for ($x = 0; $x -lt $W; $x++) {
        if ($bmp.GetPixel($x, $y).R -gt 127) {
            $i = $y * $stride + [Math]::Floor($x / 8)
            $out[$i] = $out[$i] -bor (0x80 -shr ($x % 8))
        }
    }
}
$dst = Join-Path (Split-Path $PSScriptRoot -Parent) "assets\land_${W}x${H}.bin"
[IO.File]::WriteAllBytes($dst, $out)
if ($Png) { $bmp.Save($Png, [System.Drawing.Imaging.ImageFormat]::Png) }
$bmp.Dispose()
Write-Host "Yazıldı: $dst ($($out.Length) bayt)"
