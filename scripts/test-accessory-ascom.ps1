param([switch]$Hardware, [switch]$Calibrate,
    [ValidateRange(0,10000)][int]$CalibrationPollDelayMs = 0)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$privateKeys = @()
$hive = [Microsoft.Win32.RegistryHive]::CurrentUser
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { $hive = [Microsoft.Win32.RegistryHive]::LocalMachine }
$oldId = $env:ZWOGAIN_ACCESSORY_TEST_CLSID
$oldProfile = $env:ZWOGAIN_ACCESSORY_SETTINGS
$oldWorker = $env:ZWOGAIN_ACCESSORY_WORKER
$oldSimulate = $env:ZWOGAIN_ACCESSORY_SIMULATE
try {
    dotnet build (Join-Path $repo 'src/ZwoGain.ASCOM') -c Release
    if ($LASTEXITCODE) { throw 'ASCOM build failed' }
    $testDir = Join-Path $repo ('artifacts/accessory-ascom-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $testDir | Out-Null
    $env:ZWOGAIN_ACCESSORY_SETTINGS = $testDir
    $env:ZWOGAIN_ACCESSORY_SIMULATE = if ($Hardware) { '' } else { '1' }
    $env:ZWOGAIN_ACCESSORY_WORKER = Join-Path $repo 'target/debug/zwogain-accessories.exe'
    foreach ($deviceClass in 'EfwFilterWheel','EafFocuser') {
    $env:ZWOGAIN_ACCESSORY_TEST_CLSID = [Guid]::NewGuid().ToString()
    $assemblyPath = Join-Path $repo 'src/ZwoGain.ASCOM/bin/Release/net48/ZwoGain.ASCOM.dll'
    $assemblyName = [Reflection.AssemblyName]::GetAssemblyName($assemblyPath).FullName
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $view)
        try {
            $keyPath = 'Software\Classes\CLSID\{' + $env:ZWOGAIN_ACCESSORY_TEST_CLSID + '}'
            if ($root.OpenSubKey($keyPath)) { throw 'Private test CLSID already exists' }
            $privateKeys += @{ View = $view; Path = $keyPath }
            $key = $root.CreateSubKey($keyPath + '\InprocServer32')
            try {
                $key.SetValue('', 'mscoree.dll'); $key.SetValue('ThreadingModel', 'Both')
                $key.SetValue('Class', 'ZwoGain.Ascom.' + $deviceClass); $key.SetValue('Assembly', $assemblyName)
                $key.SetValue('RuntimeVersion', 'v4.0.30319'); $key.SetValue('CodeBase', ([Uri]$assemblyPath).AbsoluteUri)
            } finally { $key.Dispose() }
        } finally { $root.Dispose() }
    }
    foreach ($architecture in 'System32','SysWOW64') {
        $arguments = @('-NoProfile','-ExecutionPolicy','Bypass','-File',(Join-Path $PSScriptRoot 'test-accessory-ascom-client.ps1'),'-DeviceClass',$deviceClass)
        if ($Hardware) { $arguments += '-Hardware' }
        if ($Calibrate) { $arguments += '-Calibrate' }
        if ($CalibrationPollDelayMs) { $arguments += @('-CalibrationPollDelayMs', $CalibrationPollDelayMs) }
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" @arguments
        if ($LASTEXITCODE) { throw "ACCESSORY COM $architecture failed" }
    }
    }
} finally {
    foreach ($entry in $privateKeys) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive, $entry.View)
        try { $root.DeleteSubKeyTree($entry.Path, $false) } finally { $root.Dispose() }
    }
    $env:ZWOGAIN_ACCESSORY_TEST_CLSID = $oldId
    $env:ZWOGAIN_ACCESSORY_SETTINGS = $oldProfile
    $env:ZWOGAIN_ACCESSORY_WORKER = $oldWorker
    $env:ZWOGAIN_ACCESSORY_SIMULATE = $oldSimulate
}
