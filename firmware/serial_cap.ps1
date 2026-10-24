# Capture the serial log from the panel, with a reset pulse and chunk arrival times.
# Temp diagnostic helper (not part of the firmware). Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File firmware\serial_cap.ps1 [-Seconds 25] [-Port COM10]
param(
    [int]$Seconds = 25,
    [string]$Port = 'COM10'
)
$ErrorActionPreference = 'Stop'
$sp = New-Object System.IO.Ports.SerialPort($Port, 115200, [System.IO.Ports.Parity]::None, 8, [System.IO.Ports.StopBits]::One)
$sp.Handshake = [System.IO.Ports.Handshake]::None
# DTR high would hold the chip in the ROM downloader; RTS is EN.
$sp.DtrEnable = $false
$sp.RtsEnable = $false
$sp.ReadTimeout = 500
$sp.Open()
$sp.DiscardInBuffer()
Start-Sleep -Milliseconds 150
$sp.RtsEnable = $true
Start-Sleep -Milliseconds 200
$t0 = Get-Date
$sp.RtsEnable = $false

# Chunk arrival times, not line times: the UART hands us bursts, and a burst
# that arrives 30 s after boot says something a per-line counter would invent.
$chunks = New-Object System.Collections.ArrayList
$total = 0
$deadline = $t0.AddSeconds($Seconds)
while ((Get-Date) -lt $deadline) {
    try {
        $n = $sp.BytesToRead
        if ($n -gt 0) {
            $b = New-Object byte[] $n
            $r = $sp.Read($b, 0, $n)
            $total += $r
            [void]$chunks.Add([pscustomobject]@{
                Ms   = [int]((Get-Date) - $t0).TotalMilliseconds
                Text = [System.Text.Encoding]::ASCII.GetString($b, 0, $r)
            })
        } else {
            Start-Sleep -Milliseconds 20
        }
    } catch { }
}
$sp.Close()

Write-Output ("=== BYTES: " + $total + " / chunks: " + $chunks.Count + " ===")
foreach ($c in $chunks) {
    $flat = $c.Text -replace "`r`n", " | " -replace "`n", " | " -replace "`r", " | "
    Write-Output ("{0,6} ms  {1}" -f $c.Ms, $flat)
}
