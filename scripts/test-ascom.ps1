$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $repo
$backend = $null
$frontend = $null
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
    # Start a private class factory, without changing installed COM registrations.
    $env:ZWOGAIN_ASCOM_TEST_CLSIDS = ((1..4 | ForEach-Object { [Guid]::NewGuid().ToString() }) -join ',')
    $frontend = Start-Process -FilePath (Join-Path $repo 'src/ZwoGain.ASCOM/bin/Release/net48/ZwoGain.ASCOM.exe') -ArgumentList '/embedding' -WindowStyle Hidden -PassThru
    Start-Sleep -Milliseconds 500
    foreach ($architecture in 'System32', 'SysWOW64') {
        for ($i = 0; $i -lt 4; $i++) {
            & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-ascom-client.ps1') -Slot $i
            if ($LASTEXITCODE) { throw "ASCOM $architecture slot $i failed" }
        }
    }
} finally {
    foreach ($child in $frontend, $backend) { if ($null -ne $child -and !$child.HasExited) { $child.Kill(); $child.WaitForExit() }; if ($null -ne $child) { $child.Dispose() } }
    $env:ZWOGAIN_ASCOM_SETTINGS = $previousSettings
    $env:ZWOGAIN_ASCOM_TEST_CLSIDS = $previousIds
    Pop-Location
}
