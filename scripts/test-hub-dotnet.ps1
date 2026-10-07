$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$workerDirectory = if ($env:REGAIN_TEST_WORKERS) { [IO.Path]::GetFullPath($env:REGAIN_TEST_WORKERS) } else { Join-Path $repo 'target/debug' }
$executable = Join-Path $workerDirectory 'regain-alpaca.exe'
if (!(Test-Path -LiteralPath $executable -PathType Leaf)) { throw 'Build regain-alpaca before running the managed hub fixtures.' }
foreach ($architecture in 'x86', 'x64') {
    $buildDirectory = Join-Path $repo "artifacts/hub-dotnet-$architecture"
    # PlatformTarget is not part of the SDK's default intermediate directory.
    # Separate it so the second build cannot reuse the first bitness's DLL.
    dotnet build (Join-Path $repo 'tests/fixtures/hub-client-net48/HubClientFixture.csproj') -c Release -warnaserror -p:PlatformTarget=$architecture -p:IntermediateOutputPath="obj/hub-dotnet-$architecture/Release/" -o $buildDirectory
    if ($LASTEXITCODE) { throw "net48 $architecture fixture build failed" }
    $bitness = if ($architecture -eq 'x86') { '32' } else { '64' }
    & (Join-Path $buildDirectory 'Regain.Hub.Client.Fixture.exe') --image-codec $bitness
    if ($LASTEXITCODE) { throw "net48 $architecture image codec fixture failed" }
    & (Join-Path $buildDirectory 'Regain.Hub.Client.Fixture.exe') --camera-image $executable $bitness
    if ($LASTEXITCODE) { throw "net48 $architecture camera image fixture failed" }
    & (Join-Path $buildDirectory 'Regain.Hub.Client.Fixture.exe') --http-scheduler $bitness
    if ($LASTEXITCODE) { throw "net48 $architecture HTTP fixture scheduler isolation failed" }
    $temporary = Join-Path $repo ('artifacts/hub-dotnet-run-' + [guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($temporary) | Out-Null
    try {
        $config = Get-Content -LiteralPath (Join-Path $repo 'crates/regain-hub/examples/simulated-observatory.json') -Raw | ConvertFrom-Json
        $config.instanceId = [guid]::NewGuid().ToString()
        $config.revision = [guid]::NewGuid().ToString()
        $configPath = Join-Path $temporary 'native hub configuration.json'
        [IO.File]::WriteAllText($configPath, ($config | ConvertTo-Json -Depth 40), [Text.UTF8Encoding]::new($false))
        & (Join-Path $buildDirectory 'Regain.Hub.Client.Fixture.exe') $executable $configPath $bitness
        if ($LASTEXITCODE) { throw "net48 $architecture hub fixture failed" }
    } finally {
        $resolved = [IO.Path]::GetFullPath($temporary)
        $allowed = [IO.Path]::GetFullPath((Join-Path $repo 'artifacts')) + [IO.Path]::DirectorySeparatorChar
        if (!$resolved.StartsWith($allowed, [StringComparison]::OrdinalIgnoreCase)) { throw 'Fixture cleanup escaped artifacts' }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
