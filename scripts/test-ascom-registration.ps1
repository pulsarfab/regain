# Run only on a disposable CI Windows machine with administrator rights.
$ErrorActionPreference = 'Stop'
$exe = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../src/ZwoGain.ASCOM/bin/Release/net48/ZwoGain.ASCOM.exe'))
if (!$env:CI) { throw 'This machine-registration check is for disposable CI runners only.' }
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
    $registration = Start-Process -FilePath $exe -ArgumentList '/unregserver' -WindowStyle Hidden -Wait -PassThru
    Get-Process ZwoGain.ASCOM -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe } | Stop-Process
    if ($registration.ExitCode) { throw 'Unregistration failed' }
}
