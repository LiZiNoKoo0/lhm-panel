# Build and flash the ST7735 text firmware, then optionally watch the serial log.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File firmware\flash.ps1
#   powershell -ExecutionPolicy Bypass -File firmware\flash.ps1 -Port COM7
#   powershell -ExecutionPolicy Bypass -File firmware\flash.ps1 -Monitor
#
# The Xtensa toolchain is not on PATH by default on this box; the env script at
# D:\esp-toolchain\esp-xtensa-env.ps1 sets cargo/rustup/espflash up in place.
param(
    [string]$Port = 'COM10',
    [switch]$Monitor
)

$ErrorActionPreference = 'Stop'

. D:\esp-toolchain\esp-xtensa-env.ps1

$project = Join-Path $PSScriptRoot 'st7735-text'
$elf = Join-Path $project 'target\xtensa-esp32-none-elf\release\st7735-text'

Push-Location $project
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $flags = @('flash', '--chip', 'esp32', '--port', $Port, '--baud', '921600')
    if ($Monitor) { $flags += '--monitor' } else { $flags += '--non-interactive' }

    # The CH343 auto-reset drops the first attempt now and then
    # ("Timeout while running ReadReg"); a second try always worked so far.
    espflash @flags $elf
    if ($LASTEXITCODE -ne 0) {
        Write-Host "espflash failed, retrying once (CH343 reset is flaky)" -ForegroundColor Yellow
        Start-Sleep -Milliseconds 500
        espflash @flags $elf
        if ($LASTEXITCODE -ne 0) { throw 'espflash failed twice' }
    }
}
finally {
    Pop-Location
}

Write-Host ""
Write-Host "flashed. now prove the pixels match the preview:" -ForegroundColor Cyan
Write-Host "  python firmware\check_parity.py --port $Port"
