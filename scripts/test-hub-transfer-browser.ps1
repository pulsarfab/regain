param([string]$ScreenshotDirectory)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$executable = Join-Path $repo 'target/debug/regain-alpaca.exe'
$temporary = Join-Path $repo ('artifacts/hub-transfer-browser-' + [guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporary) | Out-Null
$config = Get-Content -LiteralPath (Join-Path $repo 'crates/regain-hub/examples/simulated-observatory.json') -Raw | ConvertFrom-Json
if (@($config.sources | Where-Object { $_.backend.kind -ne 'simulated' }).Count) { throw 'Browser fixture requires simulated sources' }
$config.instanceId = [guid]::NewGuid().ToString(); $config.revision = [guid]::NewGuid().ToString()
$configPath = Join-Path $temporary 'hub.json'
[IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json -Depth 40), [Text.UTF8Encoding]::new($false))
$portReservation = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
$portReservation.Start(); $port = $portReservation.LocalEndpoint.Port; $portReservation.Stop()
$hubProcess = $null; $webProcess = $null
try {
    $hubProcess = Start-Process -FilePath $executable -ArgumentList @('--hub-host', '--simulate', '--hub-config', ('"' + $configPath + '"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $temporary 'host.out') -RedirectStandardError (Join-Path $temporary 'host.err')
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if ($hubProcess.HasExited) { throw 'Private simulated hub exited during startup' }
        if ((Get-Content -LiteralPath (Join-Path $temporary 'host.out') -Raw) -match 'Regain hub ready:') { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ([DateTime]::UtcNow -ge $deadline) { throw 'Private simulated hub readiness expired' }
    $webProcess = Start-Process -FilePath $executable -ArgumentList @('--simulate', '--no-discovery', '--listen', '127.0.0.1', '--port', $port, '--hub-config', ('"' + $configPath + '"'), '--profiles', ('"' + (Join-Path $temporary 'profiles.json') + '"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $temporary 'web.out') -RedirectStandardError (Join-Path $temporary 'web.err')
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    do {
        if ($webProcess.HasExited) { throw 'Private simulated HTTP server exited during startup' }
        try { $ready = Invoke-WebRequest "http://127.0.0.1:$port/setup/api/state" -TimeoutSec 1; if ($ready.StatusCode -eq 200) { break } } catch { }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ([DateTime]::UtcNow -ge $deadline) { throw 'Private HTTP readiness expired' }
    $arguments = @((Join-Path $PSScriptRoot 'test-hub-transfer-browser.cjs'), "http://127.0.0.1:$port")
    if ($ScreenshotDirectory) { $arguments += [IO.Path]::GetFullPath($ScreenshotDirectory) }
    & node @arguments
    if ($LASTEXITCODE) { throw "Browser configuration transfer failed; fixture logs: $temporary" }
} finally {
    foreach ($process in @($webProcess, $hubProcess)) {
        if ($null -ne $process) { if (!$process.HasExited) { Stop-Process -Id $process.Id -Force }; $process.Dispose() }
    }
}
