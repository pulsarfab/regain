# Run only on a disposable CI Windows machine with administrator rights.
$ErrorActionPreference = 'Stop'
if (!$env:CI) { throw 'This machine-registration check is for disposable CI runners only.' }
$stage = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../artifacts/ASCOM registration'))
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Get-ChildItem -LiteralPath (Join-Path $PSScriptRoot '../src/Regain.ASCOM.Register/bin/Release/net48') -File | Copy-Item -Destination $stage -Force
$exe = Join-Path $stage 'Regain.ASCOM.Register.exe'
Write-Output ("Activation client session {0}; user {1}" -f [Diagnostics.Process]::GetCurrentProcess().SessionId, [Security.Principal.WindowsIdentity]::GetCurrent().Name)
try {
    $registration = Start-Process -FilePath $exe -ArgumentList '/regserver' -WindowStyle Hidden -Wait -PassThru
    if ($registration.ExitCode) { throw 'Machine registration failed' }
    foreach ($architecture in 'System32','SysWOW64') {
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-fc3-ascom-client.ps1') -MetadataOnly
        if ($LASTEXITCODE) { throw 'Registered FocusCube3 COM activation failed' }
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ofp2-ascom-client.ps1') -MetadataOnly
        if ($LASTEXITCODE) { throw 'Registered OFP2 COM activation failed' }
        foreach ($deviceClass in 'EfwFilterWheel','EafFocuser') {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-accessory-ascom-client.ps1') -DeviceClass $deviceClass -MetadataOnly
            if ($LASTEXITCODE) { throw 'Registered accessory COM activation failed' }
        }
        for ($slot = 0; $slot -lt 4; $slot++) {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ascom-client.ps1') -Slot $slot -MetadataOnly
            if ($LASTEXITCODE) { throw 'Registered COM activation failed' }
        }
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-caa-ascom-client.ps1')
        if ($LASTEXITCODE) { throw 'Registered CAA COM activation failed' }
    }
} finally {
    Get-WinEvent -FilterHashtable @{LogName='System';ProviderName='Microsoft-Windows-DistributedCOM';StartTime=(Get-Date).AddMinutes(-5)} -ErrorAction SilentlyContinue | Select-Object -First 4 TimeCreated,Id,Message | Format-List
    Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA 'Regain/ASCOM/focuscube3.log') -Tail 30 -ErrorAction SilentlyContinue
    Get-CimInstance Win32_Process -Filter "Name='Regain.FocusCube.ASCOM.exe'" | Select-Object ProcessId,SessionId,CommandLine
    Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA 'Regain/ASCOM/registration.log') -Tail 30 -ErrorAction SilentlyContinue
    $registration = Start-Process -FilePath $exe -ArgumentList '/unregserver' -WindowStyle Hidden -Wait -PassThru
    if ($registration.ExitCode) { throw 'Unregistration failed' }
}
