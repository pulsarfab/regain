$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$workers = if ($env:REGAIN_TEST_WORKERS) { [IO.Path]::GetFullPath($env:REGAIN_TEST_WORKERS) } else { Join-Path $repo 'target/debug' }
& (Join-Path $PSScriptRoot 'build-hub-ascom.ps1') -Destination $workers -WarningsAsErrors
$fixture = Join-Path $repo 'artifacts/hub-com-fixture'
dotnet build (Join-Path $repo 'tests/fixtures/hub-com-driver') -c Release -o $fixture -warnaserror
if ($LASTEXITCODE) { throw 'Hub COM fixture build failed' }
dotnet build (Join-Path $repo 'tests/fixtures/hub-com-helper') -c Release -o (Join-Path $repo 'artifacts/hub-com-helper') -warnaserror
if ($LASTEXITCODE) { throw 'Hub COM helper fixture build failed' }
python (Join-Path $PSScriptRoot 'test-hub-com-worker.py') --workers $workers --fixture (Join-Path $fixture 'Regain.Hub.COM.Fixture.dll') --rust-tests
if ($LASTEXITCODE) { throw 'Hub COM worker tests failed' }
