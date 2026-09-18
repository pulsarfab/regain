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
    dotnet test tests/ZwoGain.Tests -c Release
    if ($LASTEXITCODE) { throw 'Recovery tests failed' }
    dotnet test tests/ZwoGain.NINA.Tests -c Release
    if ($LASTEXITCODE) { throw 'NINA contract tests failed' }
    & (Join-Path $PSScriptRoot 'test-ascom.ps1')
    & (Join-Path $PSScriptRoot 'test-caa-ascom.ps1')
} finally { Pop-Location }
