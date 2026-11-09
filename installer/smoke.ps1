# Smoke test for a Windows install: start the app the way a double-click does and check that it opens
# its first-run setup window and that the setup page answers. Meant for a clean machine (CI); on a
# computer that already has a vault the app opens the main window instead and this reports that.
param([Parameter(Mandatory = $true)][string]$Dir)
$ErrorActionPreference = 'Stop'

& (Join-Path $Dir 'avault.exe') --version
if ($LASTEXITCODE -ne 0) { throw 'avault.exe did not run' }

Start-Process (Join-Path $Dir 'avault-window.exe')

$deadline = (Get-Date).AddSeconds(60)
$url = $null
while ((Get-Date) -lt $deadline -and -not $url) {
    Start-Sleep -Seconds 1
    $w = Get-CimInstance Win32_Process -Filter "Name='avault-window.exe'" | Where-Object { $_.CommandLine -match 'setup\.html\?t=' } | Select-Object -First 1
    if ($w) { $url = ($w.CommandLine -split '\s+' | Where-Object { $_ -like 'http*' } | Select-Object -First 1) }
}
if (-not $url) {
    Get-Process avault* -ErrorAction SilentlyContinue | Format-Table Name, Id
    throw 'the app did not open its setup window'
}

$u = [uri]$url
$token = $u.Query.Substring(3)
$r = Invoke-RestMethod "http://$($u.Authority)/api/setup" -Headers @{ 'X-Token' = $token }
if ($null -eq $r.suggestions) { throw 'the setup page did not answer' }
$page = Invoke-WebRequest "http://$($u.Authority)/setup.html" -UseBasicParsing
if ($page.Content -notmatch 'Set up AI Session Vault') { throw 'the setup page did not load' }

Write-Output "OK: the app launched and its setup window is serving at $($u.Authority)"
Get-Process avault-window, avault -ErrorAction SilentlyContinue | Stop-Process -Force
