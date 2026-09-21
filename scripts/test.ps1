$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    cargo fmt --check
    if ($LASTEXITCODE) { throw 'Rust formatting failed' }
    cargo clippy --all-targets --locked -- -D warnings
    if ($LASTEXITCODE) { throw 'Rust lint failed' }
    cargo build --locked
    if ($LASTEXITCODE) { throw 'Test host build failed' }
    cargo test --locked
    if ($LASTEXITCODE) { throw 'Rust tests failed' }
    rustc --crate-type cdylib tests/fixtures/selection_sdk.rs -o target/debug/selection_sdk.dll
    if ($LASTEXITCODE) { throw 'SDK selection fixture build failed' }
    dotnet test tests/Regain.Tests -c Release
    if ($LASTEXITCODE) { throw 'Recovery tests failed' }
    dotnet test tests/Regain.NINA.Tests -c Release
    if ($LASTEXITCODE) { throw 'NINA contract tests failed' }
    python scripts/test-native-camera.py
    if ($LASTEXITCODE) { throw 'Native camera IPC tests failed' }
    python scripts/test-alpaca-rotator.py
    if ($LASTEXITCODE) { throw 'Alpaca rotator tests failed' }
    python scripts/test-alpaca-accessories.py
    if ($LASTEXITCODE) { throw 'Alpaca accessory tests failed' }
    python scripts/test-ofp2.py
    if ($LASTEXITCODE) { throw 'OFP2 serial and Alpaca tests failed' }
    python scripts/test-fc3.py
    if ($LASTEXITCODE) { throw 'FocusCube3 tests failed' }
    & (Join-Path $PSScriptRoot 'test-fc3-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-ofp2-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-accessory-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-caa-ascom.ps1')
} finally { Pop-Location }
