param([switch]$Hardware, [switch]$FreshProfile)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$privateKeys = @()
$hive = [Microsoft.Win32.RegistryHive]::CurrentUser
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { $hive = [Microsoft.Win32.RegistryHive]::LocalMachine }
$oldId = $env:ZWOGAIN_CAA_TEST_CLSID
$oldProfile = $env:ZWOGAIN_ROTATOR_SETTINGS
$oldWorker = $env:ZWOGAIN_CAA_WORKER
try {
    dotnet build (Join-Path $repo 'src/ZwoGain.ASCOM') -c Release
    if ($LASTEXITCODE) { throw 'ASCOM build failed' }
    $testDir = Join-Path $repo ('artifacts/caa-ascom-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $testDir | Out-Null
    $env:ZWOGAIN_ROTATOR_SETTINGS = Join-Path $testDir 'rotator.json'
    $env:ZWOGAIN_CAA_WORKER = Join-Path $repo 'target/release/zwogain-caa.exe'
    if ($Hardware) {
        $devices = @(& $env:ZWOGAIN_CAA_WORKER list-details | ConvertFrom-Json)
        if ($LASTEXITCODE -or $devices.Count -ne 1 -or !$devices[0].identity) { throw 'Exactly one available CAA required' }
        if (!$FreshProfile) { @{ Serial = $devices[0].identity.serial; LogicalOffset = 0; Synced = $false } | ConvertTo-Json | Set-Content -LiteralPath $env:ZWOGAIN_ROTATOR_SETTINGS }
    }
    $env:ZWOGAIN_CAA_TEST_CLSID = [Guid]::NewGuid().ToString()
    $assemblyPath = Join-Path $repo 'src/ZwoGain.ASCOM/bin/Release/net48/ZwoGain.ASCOM.dll'
    $assemblyName = [Reflection.AssemblyName]::GetAssemblyName($assemblyPath).FullName
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $view)
        try {
            $keyPath = 'Software\Classes\CLSID\{' + $env:ZWOGAIN_CAA_TEST_CLSID + '}'
            if ($root.OpenSubKey($keyPath)) { throw 'Private test CLSID already exists' }
            $privateKeys += @{ View = $view; Path = $keyPath }
            $key = $root.CreateSubKey($keyPath + '\InprocServer32')
            try {
                $key.SetValue('', 'mscoree.dll'); $key.SetValue('ThreadingModel', 'Both')
                $key.SetValue('Class', 'ZwoGain.Ascom.CaaRotator'); $key.SetValue('Assembly', $assemblyName)
                $key.SetValue('RuntimeVersion', 'v4.0.30319'); $key.SetValue('CodeBase', ([Uri]$assemblyPath).AbsoluteUri)
            } finally { $key.Dispose() }
        } finally { $root.Dispose() }
    }
    foreach ($architecture in 'System32','SysWOW64') {
        if ($FreshProfile) { $env:ZWOGAIN_ROTATOR_SETTINGS = Join-Path $testDir ($architecture + '-fresh.json') }
        $arguments = @('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot 'test-caa-ascom-client.ps1'))
        if ($Hardware) { $arguments += '-Hardware' }
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" @arguments
        if ($LASTEXITCODE) { throw "CAA COM $architecture failed" }
        if ($Hardware -and $FreshProfile) {
            $saved = Get-Content -LiteralPath $env:ZWOGAIN_ROTATOR_SETTINGS -Raw | ConvertFrom-Json
            if ($saved.Serial -ne $devices[0].identity.serial) { throw 'First connection did not persist the selected CAA' }
        }
    }
} finally {
    foreach ($entry in $privateKeys) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $entry.View)
        try { $root.DeleteSubKeyTree($entry.Path, $false) } finally { $root.Dispose() }
    }
    $env:ZWOGAIN_CAA_TEST_CLSID = $oldId
    $env:ZWOGAIN_ROTATOR_SETTINGS = $oldProfile
    $env:ZWOGAIN_CAA_WORKER = $oldWorker
}
