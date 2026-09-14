$ErrorActionPreference = 'Stop'
Push-Location (Join-Path $PSScriptRoot '..')
try {
    cargo fmt --check
    if ($LASTEXITCODE) { throw 'Rust formatting failed' }
    cargo clippy --all-targets --locked -- -D warnings
    if ($LASTEXITCODE) { throw 'Rust lint failed' }
    cargo test --locked
    if ($LASTEXITCODE) { throw 'Rust tests failed' }
    cargo build --locked
    if ($LASTEXITCODE) { throw 'Test host build failed' }
    dotnet test tests/ZwoGain.Tests -c Release
    if ($LASTEXITCODE) { throw 'Recovery tests failed' }
    dotnet test tests/ZwoGain.NINA.Tests -c Release
    if ($LASTEXITCODE) { throw 'NINA contract tests failed' }
} finally { Pop-Location }
