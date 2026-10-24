# Start the daemon with the token stored in daemon.token.
#
# Why a script: passing the token on the command line kept getting mangled --
# the literal text `***` reached the daemon three separate times tonight, and
# because the daemon writes the token it started with back to daemon.token, it
# also corrupted that file. Reading it here means the value never passes through
# a shell, a markdown code block, or my own typing.
param(
    [string]$Source = 'fake',
    [int]$Port = 7802
)

$ErrorActionPreference = 'Stop'
# The daemon lives in the same directory as this script; do not walk upwards,
# `H:\newwork` is the project root and its parent is the drive.
$root = $PSScriptRoot
Set-Location $root

$tokenPath = Join-Path $root 'daemon.token'
if (-not (Test-Path $tokenPath)) {
    throw "no $tokenPath; start the daemon without --token once, or create the file"
}
$token = (Get-Content $tokenPath -Raw).Trim()
if ($token.Length -lt 8) {
    throw "daemon.token looks wrong (length $($token.Length)); refusing to start"
}

$python = 'C:\Users\Administrator\AppData\Local\Programs\Python\Python312\python.exe'
Write-Host "daemon: source=$Source port=$Port token=<from daemon.token, $($token.Length) chars>"
& $python fake_daemon.py --source $Source --port $Port --token $token
