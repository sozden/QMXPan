# QMXPan: USB (UART portu) üzerinden tam yükleme.
# espflash kendi gömülü önyükleyicisini yazar ve onda OTA geri dönüşü kapalıdır.
# Bu betik bizim derlediğimiz önyükleyiciyi yazar ve OTA seçim kaydını siler
# (yoksa OTA ile seçilmiş eski bölüm açılmaya devam eder).
# Kullanım:  .\tools\flash.ps1                (COM23)
#            .\tools\flash.ps1 -Port COM7 -DerlemeYok
param(
    [string]$Port = "COM23",
    [switch]$DerlemeYok
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
. "$env:USERPROFILE\export-esp.ps1"

$elf = "D:\qpb\xtensa-esp32s3-espidf\release\qmxpan"
if (-not $DerlemeYok) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "Derleme başarısız" }
}
$boot = Get-ChildItem "D:\qpb\xtensa-esp32s3-espidf\release\build" -Recurse -Filter bootloader.bin |
    Sort-Object LastWriteTime -Descending | Select-Object -First 1
if (-not $boot) { throw "Derlenmiş bootloader.bin bulunamadı" }
Write-Host "Önyükleyici: $($boot.FullName)"

espflash flash --port $Port --chip esp32s3 --flash-size 16mb `
    --bootloader $boot.FullName `
    --partition-table partitions.csv `
    --erase-parts otadata `
    $elf
if ($LASTEXITCODE -ne 0) { throw "Yükleme başarısız" }
Write-Host "Tamam."
