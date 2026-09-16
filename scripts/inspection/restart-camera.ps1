<##
Restart exactly one ZWO camera's Windows device node. This is a research tool,
not an automatic recovery policy. Disconnect camera applications first.
Run in an elevated PowerShell. It never restarts a hub or requests a reboot.
##>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^USB\\VID_03C3&PID_[0-9A-Fa-f]{4}\\[^\\*?]+$')]
    [string]$InstanceId,
    [Parameter(Mandatory)][string]$OutputPath
)
$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Restarting a camera requires an elevated PowerShell.'
}
if (Test-Path -LiteralPath $OutputPath) { throw 'Output file already exists.' }
$camera = @(Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -ieq $InstanceId })
if ($camera.Count -ne 1 -or $camera[0].Class -ne 'Image' -or $camera[0].FriendlyName -notlike 'ZWO ASI*') {
    throw 'Expected exactly one present ZWO imaging device.'
}
$watch = [Diagnostics.Stopwatch]::StartNew()
$messages = & "$env:SystemRoot\System32\pnputil.exe" /restart-device $camera[0].InstanceId 2>&1
$code = $LASTEXITCODE
$result = [ordered]@{ operation = 'PnP device restart'; camera = $camera[0].FriendlyName;
    exitCode = $code; elapsedMs = $watch.ElapsedMilliseconds; ready = $false }
if ($code -eq 0) {
    do {
        Start-Sleep -Milliseconds 500
        $present = @(Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -ieq $InstanceId })
        if ($present.Count -eq 1 -and $present[0].Status -eq 'OK') {
            $result.ready = $true
            break
        }
    } while ($watch.Elapsed.TotalSeconds -lt 30)
}
$result.elapsedMs = $watch.ElapsedMilliseconds
# Do not publish device instance IDs from pnputil's output.
$result | ConvertTo-Json | Set-Content -LiteralPath $OutputPath -Encoding UTF8
if ($code -ne 0 -or -not $result.ready) { throw "Camera restart failed (exit $code)." }
