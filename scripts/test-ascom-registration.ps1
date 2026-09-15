# Run only on a disposable CI Windows machine with administrator rights.
$ErrorActionPreference = 'Stop'
if (!$env:CI) { throw 'This machine-registration check is for disposable CI runners only.' }
$stage = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../artifacts/ASCOM registration'))
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot '../src/ZwoGain.ASCOM.Register/bin/Release/net48') -File | Copy-Item -Destination $stage -Force
$exe = Join-Path $stage 'ZwoGain.ASCOM.Register.exe'
Write-Output ("Activation client session {0}; user {1}" -f [Diagnostics.Process]::GetCurrentProcess().SessionId, [Security.Principal.WindowsIdentity]::GetCurrent().Name)
try {
    $registration = Start-Process -FilePath $exe -ArgumentList '/regserver' -WindowStyle Hidden -Wait -PassThru
    if ($registration.ExitCode) { throw 'Machine registration failed' }
    foreach ($architecture in 'System32','SysWOW64') {
        for ($slot = 0; $slot -lt 4; $slot++) {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ascom-client.ps1') -Slot $slot -MetadataOnly
            if ($LASTEXITCODE) { throw 'Registered COM activation failed' }
        }
    }
} finally {
    Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA 'ZwoGain/ASCOM/registration.log') -Tail 30 -ErrorAction SilentlyContinue
    $registration = Start-Process -FilePath $exe -ArgumentList '/unregserver' -WindowStyle Hidden -Wait -PassThru
    if ($registration.ExitCode) { throw 'Unregistration failed' }
}
