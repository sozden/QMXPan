# QMXPan: derle, OTA imajını üret ve karta WiFi üzerinden yükle.
# Kullanım:  .\tools\ota.ps1 -Kart 192.168.1.60
#            .\tools\ota.ps1 -Kart 192.168.1.60 -DerlemeYok
param(
    [Parameter(Mandatory = $true)][string]$Kart,
    [switch]$DerlemeYok
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path $PSScriptRoot -Parent)
. "$env:USERPROFILE\export-esp.ps1"

$elf = "D:\qpb\xtensa-esp32s3-espidf\release\qmxpan"
$bin = "D:\qpb\qmxpan.bin"

if (-not $DerlemeYok) {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "Derleme başarısız" }
}
espflash save-image --chip esp32s3 --flash-size 16mb --partition-table partitions.csv $elf $bin
if ($LASTEXITCODE -ne 0) { throw "İmaj üretilemedi" }

Write-Host "Yükleniyor: $bin -> http://$Kart/api/ota"
curl.exe --fail-with-body -sS -X POST --data-binary "@$bin" -H "Content-Type: application/octet-stream" "http://$Kart/api/ota"
if ($LASTEXITCODE -ne 0) { throw "Yükleme başarısız" }
Write-Host "`nTamam, kart QMX bağlantısını kapatıp yeniden başlıyor."
