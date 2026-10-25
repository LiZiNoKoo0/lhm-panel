"""Read COM10 across the CH343 drop-outs.

`pan.ps1` gives up the moment the port closes, and on this board the port *always*
closes during boot: the CH343 falls off the device tree at the WiFi current spike
and comes back a few seconds later. Every diagnostic printed after that point --
`PAGE saved`, `PAGE restored`, the whole running phase -- was therefore invisible,
which is why a night of firmware work kept being verified by eye.

This keeps trying: open, read until it fails, sleep, open again, for the whole
window. Output is stamped with seconds since start so a gap is visible as a gap
rather than as "nothing happened" (the mistake this project already recorded once:
a log without timestamps turned 8 minutes of slow connections into "the board is
hammering the server").

Usage: py tools/serial_tolerant.py [seconds] [port]
"""

import sys
import time

try:
    import serial  # pyserial, if present
except ImportError:  # pragma: no cover
    serial = None

PORT = sys.argv[2] if len(sys.argv) > 2 else "COM10"
SECONDS = float(sys.argv[1]) if len(sys.argv) > 1 else 30.0
BAUD = 115200


def using_dotnet():
    """Fallback reader through .NET's SerialPort, which does not touch DTR/RTS.

    pyserial asserts both lines on open, and on this board DTR=IO0/RTS=EN, so it
    can hold the chip in reset and then fail every read. .NET defaults them to
    false, which is why `pan.ps1` exists at all.
    """
    import subprocess
    script = r"""
$ErrorActionPreference = 'Continue'
$port = "%s"
$deadline = (Get-Date).AddSeconds(%d)
$sw = [Diagnostics.Stopwatch]::StartNew()
while ((Get-Date) -lt $deadline) {
    $sp = $null
    try {
        $sp = New-Object System.IO.Ports.SerialPort $port,%d,None,8,one
        $sp.DtrEnable = $false
        $sp.RtsEnable = $false
        $sp.ReadTimeout = 400
        $sp.Open()
        while ((Get-Date) -lt $deadline) {
            try {
                $chunk = $sp.ReadExisting()
                if ($chunk.Length -gt 0) {
                    foreach ($line in ($chunk -split "`r?`n")) {
                        if ($line.Trim().Length -gt 0) {
                            Write-Output ("[{0,6:N1}s] {1}" -f $sw.Elapsed.TotalSeconds, $line.Trim())
                        }
                    }
                }
            } catch { break }
        }
    } catch {
        Start-Sleep -Milliseconds 400
    } finally {
        if ($sp -ne $null) { try { $sp.Close() } catch {} }
    }
}
""" % (PORT, int(SECONDS), BAUD)
    proc = subprocess.run(
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script],
        capture_output=True,
        text=True,
        errors="replace",
    )
    sys.stdout.write(proc.stdout)
    if proc.stderr.strip():
        sys.stderr.write(proc.stderr)


def main():
    if serial is None:
        using_dotnet()
        return 0
    started = time.monotonic()
    while time.monotonic() - started < SECONDS:
        try:
            with serial.Serial(PORT, BAUD, timeout=0.4) as port:
                port.dtr = False
                port.rts = False
                while time.monotonic() - started < SECONDS:
                    line = port.readline().decode("utf-8", "replace").strip()
                    if line:
                        print("[%6.1fs] %s" % (time.monotonic() - started, line), flush=True)
        except Exception:
            time.sleep(0.4)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
