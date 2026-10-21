param(
    [Parameter(Mandatory = $true)][string]$TaskFile,
    [Parameter(Mandatory = $true)][string]$LogFile
)
$ErrorActionPreference = 'Continue'
$cred = 'C:\Users\Administrator\.dsh\.credentials.yaml'
$raw = Get-Content $cred -Raw
function Get-Key($text, $name) {
    $m = [regex]::Match($text, '(?m)^\s*' + $name + ':\s*(.+?)\s*$')
    if ($m.Success) { return $m.Groups[1].Value.Trim([char[]]@(34, 39)) }
    return $null
}
$k1 = Get-Key $raw 'CLAUDE_OPUS_5_MAX_API_KEY'
if ($k1) { $env:CLAUDE_OPUS_5_MAX_API_KEY = $k1 }
$k2 = Get-Key $raw 'FENGJIN_API_KEY'
if ($k2) { $env:FENGJIN_API_KEY = $k2 }
if (-not $k1 -and -not $k2) { Write-Output 'NO KEYS FOUND'; exit 1 }
Set-Location 'H:\newwork'
$dsh = 'C:\Users\Administrator\AppData\Local\npm-cache\_npx\1e7f6d9597241db0\node_modules\.bin\dsh.cmd'
$prompt = Get-Content $TaskFile -Raw
& $dsh headless $prompt *> $LogFile
Write-Output ('EXIT=' + $LASTEXITCODE)
