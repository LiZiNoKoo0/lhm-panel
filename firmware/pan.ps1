# Read the ST7735 panel's serial log without pyserial.
#
# Why not python: pyserial 3.5 on this box leaves DTR/RTS asserted when it opens
# the port, which holds the board in reset, and then its first read dies with
# ClearCommError (ERROR_INVALID_FUNCTION). .NET's SerialPort defaults both lines
# to false, so it opens a running board without touching the EN pin. That is the
# whole reason this file exists.
#
# usage:
#   powershell -NoProfile -File firmware\pan.ps1                 # 12 s capture
#   powershell -NoProfile -File firmware\pan.ps1 -Seconds 30
#   powershell -NoProfile -File firmware\pan.ps1 -Port COM10 -Reset
#   powershell -NoProfile -File firmware\pan.ps1 -Lines "CPU 33.5%|TEMP   34C"
#
# -Reset pulses RTS (EN) for 150 ms, then releases it -- the same shape esptool
# uses, including the extra DTR write the usbser.sys driver needs. Pass it only
# when you want a fresh boot: opening the port normally does NOT reset anything.
param(
    [string]$Port = 'COM10',
    [int]$Seconds = 12,
    [string[]]$Lines = @(),
    [switch]$Reset
)

$ErrorActionPreference = 'Stop'

$sp = New-Object System.IO.Ports.SerialPort $Port, 115200
$sp.DtrEnable = $false
$sp.RtsEnable = $false
$sp.ReadTimeout = 200
$sp.Open()

try {
    if ($Reset) {
        $sp.RtsEnable = $true
        $sp.DtrEnable = $false
        Start-Sleep -Milliseconds 150
        $sp.RtsEnable = $false
        $sp.DtrEnable = $false
        Start-Sleep -Milliseconds 100
    }

    $buf = New-Object System.Text.StringBuilder
    $deadline = (Get-Date).AddSeconds($Seconds)
    $nextSend = Get-Date
    while ((Get-Date) -lt $deadline) {
        $chunk = $sp.ReadExisting()
        if ($chunk.Length -gt 0) {
            [void]$buf.Append($chunk)
            Write-Host -NoNewline $chunk
        }
        # Content lines are re-sent on a timer, not once: the first write after
        # opening the port is occasionally eaten (see BACKLOG, DTR/RTS notes).
        if ($Lines.Count -gt 0 -and (Get-Date) -ge $nextSend) {
            foreach ($line in $Lines) {
                $sp.Write($line + "`n")
            }
            $nextSend = (Get-Date).AddSeconds(2)
        }
        Start-Sleep -Milliseconds 60
    }

    Write-Host ""
    Write-Host ("---- {0} bytes ----" -f $buf.Length) -ForegroundColor DarkGray
}
finally {
    $sp.Close()
}
