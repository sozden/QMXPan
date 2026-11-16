# Kartı RESETLEMEDEN seri kaydı dosyaya yazar (DTR/RTS kapalı).
# Kullanım: tools\serlog.ps1 -Port COM23 -Seconds 120 -Out D:\QMXPan\ser.log
param(
    [string]$Port = "COM23",
    [int]$Seconds = 120,
    [string]$Out = "D:\QMXPan\ser.log"
)
$sp = New-Object System.IO.Ports.SerialPort $Port, 115200, None, 8, One
$sp.DtrEnable = $false
$sp.RtsEnable = $false
$sp.ReadTimeout = 500
$sp.Open()
$end = (Get-Date).AddSeconds($Seconds)
$w = New-Object System.IO.StreamWriter($Out, $false, [System.Text.Encoding]::UTF8)
try {
    while ((Get-Date) -lt $end) {
        $n = $sp.BytesToRead
        if ($n -gt 0) {
            $buf = New-Object byte[] $n
            $null = $sp.Read($buf, 0, $n)
            $w.Write([System.Text.Encoding]::UTF8.GetString($buf))
            $w.Flush()
        }
        Start-Sleep -Milliseconds 50
    }
} finally {
    $w.Close()
    $sp.Close()
}
