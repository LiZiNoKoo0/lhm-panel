# Build pcmon_bridge.exe against a local LibreHardwareMonitor install.
#
# The bridge must be built INSIDE the LibreHardwareMonitor folder, because at
# runtime it needs LibreHardwareMonitorLib.dll and its native/Managed friends
# (HidSharp, RAMSPDToolkit, ...) from that same directory.
#
#   powershell -ExecutionPolicy Bypass -File build.ps1
#   powershell -ExecutionPolicy Bypass -File build.ps1 -LhmDir 'D:\other\lhm'
#
# The C# compiler ships with .NET Framework 4 - nothing to install.

param(
    [string]$LhmDir = 'H:\tools\lhm'
)

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$source = Join-Path $here 'pcmon_bridge.cs'
$lib = Join-Path $LhmDir 'LibreHardwareMonitorLib.dll'
$csc = 'C:\Windows\Microsoft.NET\Framework64\v4.0.30319\csc.exe'
$out = Join-Path $LhmDir 'lhm_bridge.exe'

if (-not (Test-Path $csc)) { throw "csc.exe not found at $csc" }
if (-not (Test-Path $lib)) {
    throw "LibreHardwareMonitorLib.dll not found in $LhmDir. Download LibreHardwareMonitor first: https://github.com/LibreHardwareMonitor/LibreHardwareMonitor/releases"
}

& $csc /nologo /target:exe /out:$out /r:$lib $source
if ($LASTEXITCODE -ne 0) { throw "csc failed with $LASTEXITCODE" }
Write-Output "built $out"
