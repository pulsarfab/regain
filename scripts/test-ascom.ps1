$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $repo
$backend = $null
$privateKeys = @()
$previousSettings = $env:ZWOGAIN_ASCOM_SETTINGS
$previousIds = $env:ZWOGAIN_ASCOM_TEST_CLSIDS
try {
    dotnet build src/ZwoGain.ASCOM -c Release
    if ($LASTEXITCODE) { throw 'ASCOM build failed' }
    $testDir = Join-Path $repo ('artifacts/ascom-test-' + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $testDir | Out-Null
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    $url = "http://127.0.0.1:$port"
    $backend = Start-Process -FilePath (Join-Path $repo 'target/debug/zwogain-alpaca.exe') -ArgumentList '--simulate','--no-discovery','--port',"$port",'--profiles',('"' + (Join-Path $testDir 'cameras.json') + '"') -WindowStyle Hidden -PassThru -RedirectStandardError (Join-Path $testDir 'rust.log')
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        try { $state = Invoke-RestMethod "$url/setup/api/state"; break } catch {
            if ($backend.HasExited -or [DateTime]::UtcNow -gt $deadline) { throw }
            Start-Sleep -Milliseconds 100
        }
    } while ($true)
    for ($i = $state.cameras.Count; $i -lt 4; $i++) { Invoke-RestMethod "$url/setup/api/slots" -Method Post -ContentType 'application/json' -Body '{}' | Out-Null }
    $sdk = (Invoke-RestMethod "$url/setup/api/discover" -Method Post -ContentType 'application/json' -Body '{"direct":false}')
    $direct = (Invoke-RestMethod "$url/setup/api/discover" -Method Post -ContentType 'application/json' -Body '{"direct":true}')
    $state = Invoke-RestMethod "$url/setup/api/state"
    for ($i = 0; $i -lt 4; $i++) {
        $p = $state.cameras[$i].profile
        $p.camera = if ($i -eq 0) { $sdk[0] } else { $direct[$i] }
        $p.direct = $i -ne 0
        $p.recovery.reconnectDelaySeconds = 0.05
        Invoke-RestMethod "$url/setup/api/cameras/$i" -Method Post -ContentType 'application/json' -Body ($p | ConvertTo-Json -Depth 20) | Out-Null
    }
    $env:ZWOGAIN_ASCOM_SETTINGS = Join-Path $testDir 'server.json'
    @{ Address = '127.0.0.1'; Port = $port; StartLocalServer = $false } | ConvertTo-Json | Set-Content -LiteralPath $env:ZWOGAIN_ASCOM_SETTINGS
    # Private per-user CLSIDs exercise the actual COM DLL loader without touching
    # installed camera entries or requiring administrator rights.
    $env:ZWOGAIN_ASCOM_TEST_CLSIDS = ((1..4 | ForEach-Object { [Guid]::NewGuid().ToString() }) -join ',')
    $assemblyPath = Join-Path $repo 'src/ZwoGain.ASCOM/bin/Release/net48/ZwoGain.ASCOM.dll'
    $assemblyName = [Reflection.AssemblyName]::GetAssemblyName($assemblyPath).FullName
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $view)
        try {
            for ($i = 0; $i -lt 4; $i++) {
                $keyPath = 'Software\Classes\CLSID\{' + $env:ZWOGAIN_ASCOM_TEST_CLSIDS.Split(',')[$i] + '}'
                if ($root.OpenSubKey($keyPath)) { throw 'Test CLSID already exists' }
                $privateKeys += @{ View = $view; Path = $keyPath }
                $key = $root.CreateSubKey($keyPath + '\InprocServer32')
                try {
                    $key.SetValue('', 'mscoree.dll')
                    $key.SetValue('ThreadingModel', 'Both')
                    $key.SetValue('Class', ('ZwoGain.Ascom.Camera' + ($i + 1)))
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
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::CurrentUser, $entry.View)
        try { $root.DeleteSubKeyTree($entry.Path, $false) } finally { $root.Dispose() }
    }
    if ($null -ne $backend) { if (!$backend.HasExited) { $backend.Kill(); $backend.WaitForExit() }; $backend.Dispose() }
    $env:ZWOGAIN_ASCOM_SETTINGS = $previousSettings
    $env:ZWOGAIN_ASCOM_TEST_CLSIDS = $previousIds
    Pop-Location
}
