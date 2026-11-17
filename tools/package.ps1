# QMXPan: Full ve Lite paketlerini derler, dağıtım dosyalarını dist\ altına yazar.
#
#   qmxpan-full-<sürüm>.bin      tam imaj (önyükleyici + bölümler + uygulama), 0x0'a yazılır
#   qmxpan-full-ota-<sürüm>.bin  yalnız uygulama: web sayfasından güncelleme için
#   qmxpan-lite-<sürüm>.bin      tam imaj, 0x0'a yazılır
#   web\                         tarayıcıdan yükleme sayfası için kopyalar ve manifestler
#
# Kullanım:  .\tools\package.ps1
param(
    [string]$Target = "D:\qpb"
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
if (Test-Path "$env:USERPROFILE\export-esp.ps1") { . "$env:USERPROFILE\export-esp.ps1" }

$ver = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$rel = Join-Path $Target "xtensa-esp32s3-espidf\release"
$elf = Join-Path $rel "qmxpan"
$dist = Join-Path (Get-Location) "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
New-Item -ItemType Directory -Force (Join-Path $dist "web") | Out-Null

function Build($features) {
    if ($features -eq "lite") { cargo build --release --no-default-features }
    else { cargo build --release }
    if ($LASTEXITCODE -ne 0) { throw "Derleme başarısız ($features)" }
}

function Bootloader {
    $b = Get-ChildItem (Join-Path $rel "build") -Recurse -Filter bootloader.bin |
        Sort-Object LastWriteTime -Descending | Select-Object -First 1
    if (-not $b) { throw "Derlenmiş bootloader.bin bulunamadı" }
    $b.FullName
}

$web = Join-Path $dist "web"

# Paketin dosyaları:
#  - birleşik tam imaj (0x0): tek dosya, esptool/espflash ile elle yükleyenler için.
#    Boşlukları 0xFF ile doldurduğundan ayarları (NVS) da siler.
#  - web yükleyici parçaları: önyükleyici, bölüm tablosu, boş otadata, uygulama.
#    NVS'e dokunmaz; güncellemede WiFi, çağrı işareti ve ayarlar korunur.
function Pack($name) {
    $out = Join-Path $dist "qmxpan-$name-$ver.bin"
    espflash save-image --chip esp32s3 --flash-size 16mb --merge --skip-padding `
        --bootloader (Bootloader) --partition-table partitions.csv $elf $out
    if ($LASTEXITCODE -ne 0) { throw "Birleşik imaj üretilemedi ($name)" }
    $app = Join-Path $web "$name-app.bin"
    espflash save-image --chip esp32s3 --flash-size 16mb --partition-table partitions.csv $elf $app
    if ($LASTEXITCODE -ne 0) { throw "Uygulama imajı üretilemedi ($name)" }
    Copy-Item (Bootloader) (Join-Path $web "$name-bootloader.bin") -Force
}

Write-Host "== Full $ver"
Build "full"
Pack "full"
Copy-Item (Join-Path $web "full-app.bin") (Join-Path $dist "qmxpan-full-ota-$ver.bin") -Force

Write-Host "== Lite $ver"
Build "lite"
Pack "lite"

# Bölüm tablosu ve boş otadata (ota_0'dan açılsın) her iki pakette ortak
espflash partition-table partitions.csv --to-binary -o (Join-Path $web "partitions.bin")
if ($LASTEXITCODE -ne 0) { throw "Bölüm tablosu üretilemedi" }
[IO.File]::WriteAllBytes((Join-Path $web "otadata-blank.bin"), [byte[]](,0xFF * 0x2000))

# ESP Web Tools manifestleri (tarayıcıdan yükleme sayfası)
foreach ($n in @("full", "lite")) {
    $title = if ($n -eq "full") { "QMXPan Full" } else { "QMXPan Lite" }
    $m = @"
{
  "name": "$title",
  "version": "$ver",
  "new_install_prompt_erase": true,
  "builds": [
    {
      "chipFamily": "ESP32-S3",
      "parts": [
        { "path": "$n-bootloader.bin", "offset": 0 },
        { "path": "partitions.bin", "offset": 32768 },
        { "path": "otadata-blank.bin", "offset": 61440 },
        { "path": "$n-app.bin", "offset": 131072 }
      ]
    }
  ]
}
"@
    [IO.File]::WriteAllText((Join-Path $dist "web\manifest-$n.json"), $m, (New-Object Text.UTF8Encoding($false)))
}

Get-ChildItem $dist -File | ForEach-Object { "{0,-34} {1,10:N0} B" -f $_.Name, $_.Length }
Write-Host "Tamam: $dist"
