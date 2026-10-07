$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$workers = if ($env:REGAIN_TEST_WORKERS) { [IO.Path]::GetFullPath($env:REGAIN_TEST_WORKERS) } else { Join-Path $repo 'target/debug' }
& (Join-Path $PSScriptRoot 'build-hub-ascom.ps1') -Destination $workers -WarningsAsErrors
$fixture = Join-Path $repo 'artifacts/hub-com-fixture'
dotnet build (Join-Path $repo 'tests/fixtures/hub-com-driver') -c Release -o $fixture -warnaserror
if ($LASTEXITCODE) { throw 'Hub COM fixture build failed' }
dotnet build (Join-Path $repo 'tests/fixtures/hub-com-helper') -c Release -o (Join-Path $repo 'artifacts/hub-com-helper') -warnaserror
if ($LASTEXITCODE) { throw 'Hub COM helper fixture build failed' }
$fixtureArguments = @((Join-Path $PSScriptRoot 'test-hub-com-worker.py'), '--workers', $workers, '--fixture', (Join-Path $fixture 'Regain.Hub.COM.Fixture.dll'), '--rust-tests', '--nina-tests')
$elevated = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
# Elevated COM ignores HKCU registrations. Opt in only on the disposable runner;
# Python verifies this context too and checks both hives before writing any key.
if ($env:GITHUB_ACTIONS -eq 'true' -and $env:RUNNER_OS -eq 'Windows' -and $elevated) { $fixtureArguments += '--machine-fixture' }
& python @fixtureArguments
if ($LASTEXITCODE) { throw 'Hub COM worker tests failed' }
