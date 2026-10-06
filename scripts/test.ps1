$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    foreach ($architecture in 'System32','SysWOW64') {
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-com-property.ps1')
        if ($LASTEXITCODE) { throw 'Strict COM property tests failed' }
    }
    cargo fmt --check
    if ($LASTEXITCODE) { throw 'Rust formatting failed' }
    cargo clippy --all-targets --locked -- -D warnings
    if ($LASTEXITCODE) { throw 'Rust lint failed' }
    cargo build --locked
    if ($LASTEXITCODE) { throw 'Test host build failed' }
    python scripts/test-device.py
    if ($LASTEXITCODE) { throw 'Unified device CLI tests failed' }
    $previousWorkerDirectory = $env:REGAIN_TEST_WORKERS
    try {
        $env:REGAIN_TEST_WORKERS = (Resolve-Path target/debug).Path
        cargo test --locked
        if ($LASTEXITCODE) { throw 'Rust tests failed' }
    } finally { $env:REGAIN_TEST_WORKERS = $previousWorkerDirectory }
    rustc --crate-type cdylib tests/fixtures/selection_sdk.rs -o target/debug/selection_sdk.dll
    if ($LASTEXITCODE) { throw 'SDK selection fixture build failed' }
    dotnet test tests/Regain.Tests -c Release
    if ($LASTEXITCODE) { throw 'Recovery tests failed' }
    dotnet test tests/Regain.NINA.Tests -c Release
    if ($LASTEXITCODE) { throw 'NINA contract tests failed' }
    python scripts/test-native-camera.py
    if ($LASTEXITCODE) { throw 'Native camera IPC tests failed' }
    python scripts/test-usb-recovery.py
    if ($LASTEXITCODE) { throw 'USB recovery tests failed' }
    python scripts/test-alpaca-rotator.py
    if ($LASTEXITCODE) { throw 'Alpaca rotator tests failed' }
    python scripts/test-alpaca-falcon.py
    if ($LASTEXITCODE) { throw 'Falcon Alpaca tests failed' }
    & (Join-Path $PSScriptRoot 'test-falcon-ascom.ps1')
    python scripts/test-alpaca-accessories.py
    if ($LASTEXITCODE) { throw 'Alpaca accessory tests failed' }
    python scripts/test-alpaca-focusers.py
    if ($LASTEXITCODE) { throw 'Dynamic focuser tests failed' }
    python scripts/test-ofp2.py
    if ($LASTEXITCODE) { throw 'OFP2 serial and Alpaca tests failed' }
    python scripts/test-fc3.py
    if ($LASTEXITCODE) { throw 'FocusCube3 tests failed' }
    & (Join-Path $PSScriptRoot 'test-fc3-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-ofp2-ascom.ps1')
    # Recreate the server for every iteration; both architectures make their
    # first metadata request together. These are assertions, not test retries.
    foreach ($iteration in 1..20) {
        Write-Output "OFP2 cold metadata iteration $iteration/20"
        & (Join-Path $PSScriptRoot 'test-ofp2-ascom.ps1') -MetadataOnly -SkipBuild
    }
    python scripts/test-eta.py
    if ($LASTEXITCODE) { throw 'ETA tests failed' }
    & (Join-Path $PSScriptRoot 'test-eta-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-accessory-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-caa-ascom.ps1')
} finally { Pop-Location }
