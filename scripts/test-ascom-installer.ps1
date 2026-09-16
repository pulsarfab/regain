# Machine-wide registration and prerequisite fixtures: disposable CI only.
$ErrorActionPreference = 'Stop'
if (!$env:CI -or !([Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { throw 'Run on a disposable, elevated CI runner.' }
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$version = & (Join-Path $PSScriptRoot 'version.ps1')
$installer = Join-Path $repo "artifacts/ZwoGain-ASCOM-$version-win-x64-setup.exe"
$testDir = Join-Path $repo 'artifacts/installer-test'
$destination = Join-Path $testDir 'Installed ASCOM'
$uninstallKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{6C6E7298-5281-4CB4-92F0-0C3702B8BFAA}_is1'
$platformKey = 'HKLM:\SOFTWARE\WOW6432Node\ASCOM'
if (Test-Path $uninstallKey) { throw 'An installation already exists' }
function Assert-NoCameraEntries {
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::LocalMachine, $view)
        try {
            for ($slot = 1; $slot -le 4; $slot++) {
                foreach ($path in "Software\Classes\CLSID\{D1DB6F94-5CC0-4752-A758-F849098874A$slot}","Software\ASCOM\Camera Drivers\ASCOM.ZWOgain.Camera$slot") {
                    $key = $root.OpenSubKey($path)
                    if ($key) { $key.Dispose(); throw "Camera entry exists in $view : $path" }
                }
            }
        } finally { $root.Dispose() }
    }
}
Assert-NoCameraEntries
$platformBefore = Get-ItemPropertyValue $platformKey -Name PlatformVersion -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $testDir -Force | Out-Null
$oldSettings = $env:ZWOGAIN_ASCOM_SETTINGS
$oldIds = $env:ZWOGAIN_ASCOM_TEST_CLSIDS
$env:ZWOGAIN_ASCOM_TEST_CLSIDS = $null
$env:ZWOGAIN_ASCOM_SETTINGS = Join-Path $testDir 'server.json'
$backend = $null
function Run-Setup([string]$Label, [bool]$Success = $true, [string]$Directory = $destination) {
    $p = Start-Process -FilePath $installer -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',('/DIR="' + $Directory + '"'),('/LOG="' + (Join-Path $testDir "$Label.log") + '"') -WindowStyle Hidden -Wait -PassThru
    if (($p.ExitCode -eq 0) -ne $Success) { throw "$Label returned $($p.ExitCode); see installer-test logs" }
}
function Run-Uninstall([string]$Label, [bool]$Success = $true) {
    $p = Start-Process -FilePath (Join-Path $destination 'unins000.exe') -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART',('/LOG="' + (Join-Path $testDir "$Label.log") + '"') -WindowStyle Hidden -Wait -PassThru
    if (($p.ExitCode -eq 0) -ne $Success) { throw "$Label returned $($p.ExitCode)" }
}
try {
    # ASCOM is not needed for these self-contained COM classes. Its registry
    # version is a fixture so the production prerequisite gate is exercised.
    Remove-ItemProperty $platformKey -Name PlatformVersion -ErrorAction SilentlyContinue
    Run-Setup 'missing-platform' $false
    if (Test-Path (Join-Path $destination 'ZwoGain.ASCOM.dll')) { throw 'Prerequisite failure installed files' }
    New-Item $platformKey -Force | Out-Null
    Set-ItemProperty $platformKey -Name PlatformVersion -Value '7.1'
    # Deny one real registry write to exercise Inno's transactional rollback.
    $blockedPath = 'HKLM:\SOFTWARE\Classes\CLSID\{D1DB6F94-5CC0-4752-A758-F849098874A1}'
    New-Item -Path $blockedPath -Force | Out-Null
    $originalAcl = Get-Acl $blockedPath
    $deniedAcl = Get-Acl $blockedPath
    $rule = [Security.AccessControl.RegistryAccessRule]::new([Security.Principal.WindowsIdentity]::GetCurrent().User, [Security.AccessControl.RegistryRights]::SetValue, [Security.AccessControl.AccessControlType]::Deny)
    $deniedAcl.AddAccessRule($rule)
    try {
        Set-Acl $blockedPath $deniedAcl
        Run-Setup 'registration-failure' $false
        if ((Test-Path $uninstallKey) -or (Test-Path (Join-Path $destination 'ZwoGain.ASCOM.dll'))) { throw 'Failed registration did not roll back installation' }
    } finally {
        if (Test-Path $blockedPath) {
            Set-Acl $blockedPath $originalAcl
            Remove-Item -LiteralPath $blockedPath -Force
        }
    }
    Assert-NoCameraEntries
    Run-Setup 'install'
    $registered = Get-ItemPropertyValue 'HKLM:\SOFTWARE\Classes\CLSID\{D1DB6F94-5CC0-4752-A758-F849098874A1}\InprocServer32' -Name CodeBase
    if (([Uri]$registered).LocalPath -ne (Join-Path $destination 'ZwoGain.ASCOM.dll')) { throw 'Wrong installed registration path' }
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop()
    $url = "http://127.0.0.1:$port"
    $profiles = Join-Path $testDir 'cameras.json'
    $backend = Start-Process -FilePath (Join-Path $destination 'zwogain-alpaca.exe') -ArgumentList '--simulate','--no-discovery','--port',"$port",'--profiles',('"' + $profiles + '"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $testDir 'server.log')
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    do {
        try { $state = Invoke-RestMethod "$url/setup/api/state"; break } catch {
            if ($backend.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw }
            Start-Sleep -Milliseconds 100
        }
    } while ($true)
    for ($i = $state.cameras.Count; $i -lt 4; $i++) { Invoke-RestMethod "$url/setup/api/slots" -Method Post -ContentType 'application/json' -Body '{}' | Out-Null }
    $cameras = Invoke-RestMethod "$url/setup/api/discover" -Method Post -ContentType 'application/json' -Body '{"direct":false}'
    $state = Invoke-RestMethod "$url/setup/api/state"
    for ($i = 0; $i -lt 4; $i++) {
        $profile = $state.cameras[$i].profile
        $profile.camera = $cameras[0]
        $profile.direct = $false
        Invoke-RestMethod "$url/setup/api/cameras/$i" -Method Post -ContentType 'application/json' -Body ($profile | ConvertTo-Json -Depth 20) | Out-Null
    }
    @{ Address = '127.0.0.1'; Port = $port; StartLocalServer = $false } | ConvertTo-Json | Set-Content -LiteralPath $env:ZWOGAIN_ASCOM_SETTINGS
    foreach ($architecture in 'System32','SysWOW64') {
        for ($slot = 0; $slot -lt 4; $slot++) {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ascom-client.ps1') -Slot $slot
            if ($LASTEXITCODE) { throw 'Installed COM capture failed' }
        }
    }
    $settingsHash = (Get-FileHash $env:ZWOGAIN_ASCOM_SETTINGS).Hash
    $profilesHash = (Get-FileHash $profiles).Hash
    Run-Setup 'busy-upgrade' $false
    Run-Uninstall 'busy-uninstall' $false
    if ($backend.HasExited) { throw 'Installer stopped the running server' }
    $backend.Kill(); $backend.WaitForExit(); $backend.Dispose(); $backend = $null
    Start-Sleep -Seconds 2
    Run-Setup 'upgrade'
    Run-Setup 'moved-upgrade' $false (Join-Path $testDir 'Other directory')
    Set-ItemProperty $uninstallKey -Name DisplayVersion -Value '99.0.0.0'
    Run-Setup 'downgrade' $false
    Set-ItemProperty $uninstallKey -Name DisplayVersion -Value $version
    foreach ($architecture in 'System32','SysWOW64') {
        for ($slot = 0; $slot -lt 4; $slot++) {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ascom-client.ps1') -Slot $slot -MetadataOnly
            if ($LASTEXITCODE) { throw 'Upgraded COM activation failed' }
        }
    }
    Run-Uninstall 'uninstall'
    if ((Test-Path $uninstallKey) -or (Test-Path (Join-Path $destination 'ZwoGain.ASCOM.dll'))) { throw 'Uninstall left application files or entry' }
    Assert-NoCameraEntries
    if ((Get-FileHash $env:ZWOGAIN_ASCOM_SETTINGS).Hash -ne $settingsHash -or (Get-FileHash $profiles).Hash -ne $profilesHash) { throw 'Setup changed user settings' }
    Write-Output 'Installer: prerequisites, 8 COM captures, busy guards, upgrade, downgrade guard, uninstall and settings preservation passed.'
} finally {
    if ($backend -and !$backend.HasExited) { $backend.Kill(); $backend.WaitForExit() }
    if ($platformBefore) { Set-ItemProperty $platformKey -Name PlatformVersion -Value $platformBefore }
    else { Remove-ItemProperty $platformKey -Name PlatformVersion -ErrorAction SilentlyContinue }
    $env:ZWOGAIN_ASCOM_SETTINGS = $oldSettings
    $env:ZWOGAIN_ASCOM_TEST_CLSIDS = $oldIds
}
