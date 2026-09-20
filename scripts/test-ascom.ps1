$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $repo
$backend = $null
$privateKeys = @()
$registryHive = [Microsoft.Win32.RegistryHive]::CurrentUser
# COM can ignore per-user registrations in elevated/UAC-disabled clients.
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { $registryHive = [Microsoft.Win32.RegistryHive]::LocalMachine }
$previousSettings = $env:REGAIN_ASCOM_PROFILES
$previousSimulation = $env:REGAIN_ASCOM_SIMULATE
$previousWorker = $env:REGAIN_CAMERA_WORKER
$previousIds = $env:REGAIN_ASCOM_TEST_CLSIDS
try {
    dotnet build src/Regain.ASCOM -c Release
    if ($LASTEXITCODE) { throw 'ASCOM build failed' }
    $testDir = Join-Path $repo ('artifacts/ascom-test-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $testDir | Out-Null
    $env:REGAIN_ASCOM_PROFILES = $testDir
    $env:REGAIN_ASCOM_SIMULATE = '1'
    $env:REGAIN_CAMERA_WORKER = Join-Path $repo 'target/debug/regain-camera.exe'
    python scripts/test-native-camera.py --prepare $testDir
    if ($LASTEXITCODE) { throw 'Native camera profile preparation failed' }
    # Private CLSIDs exercise the actual COM DLL loader without touching installed
    # camera entries. Ordinary local tests do not need administrator rights.
    $env:REGAIN_ASCOM_TEST_CLSIDS = ((1..4 | ForEach-Object { [Guid]::NewGuid().ToString() }) -join ',')
    $assemblyPath = Join-Path $repo 'src/Regain.ASCOM/bin/Release/net48/Regain.ASCOM.dll'
    $assemblyName = [Reflection.AssemblyName]::GetAssemblyName($assemblyPath).FullName
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($registryHive, $view)
        try {
            for ($i = 0; $i -lt 4; $i++) {
                $keyPath = 'Software\Classes\CLSID\{' + $env:REGAIN_ASCOM_TEST_CLSIDS.Split(',')[$i] + '}'
                if ($root.OpenSubKey($keyPath)) { throw 'Test CLSID already exists' }
                $privateKeys += @{ View = $view; Path = $keyPath }
                $key = $root.CreateSubKey($keyPath + '\InprocServer32')
                try {
                    $key.SetValue('', 'mscoree.dll')
                    $key.SetValue('ThreadingModel', 'Both')
                    $key.SetValue('Class', ('Regain.Ascom.Camera' + ($i + 1)))
                    $key.SetValue('Assembly', $assemblyName)
                    $key.SetValue('RuntimeVersion', 'v4.0.30319')
                    $key.SetValue('CodeBase', ([Uri]$assemblyPath).AbsoluteUri)
                } finally { $key.Dispose() }
            }
        } finally { $root.Dispose() }
    }
    foreach ($architecture in 'System32', 'SysWOW64') {
        for ($i = 0; $i -lt 4; $i++) {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ascom-client.ps1') -Slot $i
            if ($LASTEXITCODE) { throw "ASCOM $architecture slot $i failed" }
        }
    }
} finally {
    foreach ($entry in $privateKeys) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($registryHive, $entry.View)
        try { $root.DeleteSubKeyTree($entry.Path, $false) } finally { $root.Dispose() }
    }
    if ($null -ne $backend) { if (!$backend.HasExited) { $backend.Kill(); $backend.WaitForExit() }; $backend.Dispose() }
    $env:REGAIN_ASCOM_PROFILES = $previousSettings
    $env:REGAIN_ASCOM_SIMULATE = $previousSimulation
    $env:REGAIN_CAMERA_WORKER = $previousWorker
    $env:REGAIN_ASCOM_TEST_CLSIDS = $previousIds
    Pop-Location
}
