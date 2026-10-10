param([string]$ScreenshotDirectory)
$ErrorActionPreference='Stop'
$repo=[IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$temporary=Join-Path $repo ('artifacts/camera-recovery-browser-'+[guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($temporary)|Out-Null
$listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,0)
$listener.Start();$port=$listener.LocalEndpoint.Port;$listener.Stop()
$server=$null
try {
    $server=Start-Process -FilePath (Join-Path $repo 'target/debug/regain-alpaca.exe') -ArgumentList @('--simulate','--no-discovery','--listen','127.0.0.1','--port',$port,'--profiles',('"'+(Join-Path $temporary 'profiles.json')+'"')) -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $temporary 'server.out') -RedirectStandardError (Join-Path $temporary 'server.err')
    $deadline=[DateTime]::UtcNow.AddSeconds(15)
    do {
        if($server.HasExited){throw 'Simulated server exited during startup'}
        try {$response=Invoke-WebRequest "http://127.0.0.1:$port/setup/api/state" -TimeoutSec 1;if($response.StatusCode -eq 200){break}} catch { }
        Start-Sleep -Milliseconds 100
    } while([DateTime]::UtcNow -lt $deadline)
    if([DateTime]::UtcNow -ge $deadline){throw 'Simulation readiness expired'}
    $arguments=@((Join-Path $PSScriptRoot 'test-camera-recovery-browser.cjs'),"http://127.0.0.1:$port")
    if($ScreenshotDirectory){$arguments+=[IO.Path]::GetFullPath($ScreenshotDirectory)}
    & node @arguments
    if($LASTEXITCODE){throw "Camera recovery browser checks failed; logs: $temporary"}
} finally {if($null -ne $server){if(!$server.HasExited){Stop-Process -Id $server.Id -Force};$server.Dispose()}}
