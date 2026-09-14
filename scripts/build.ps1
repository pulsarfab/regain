[CmdletBinding()]
param([switch]$Install)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $repo
try {
    cargo build --release --locked
    if ($LASTEXITCODE) { throw 'Rust build failed' }
    dotnet build src/ZwoGain.NINA -c Release
    if ($LASTEXITCODE) { throw 'Plugin build failed' }
    $out = Join-Path $repo 'artifacts'
    $stage = Join-Path $out ('package-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Force $stage | Out-Null
    foreach ($file in @('ZwoGain.NINA.dll','ZwoGain.Core.dll')) {
        Copy-Item -LiteralPath (Join-Path $repo "src/ZwoGain.NINA/bin/Release/net8.0-windows7.0/$file") -Destination $stage
    }
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-host.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'vendor/zwo/ASICamera2.dll') -Destination $stage
    foreach ($file in @('LICENSE','README.md','THIRD_PARTY_NOTICES.md')) { Copy-Item -LiteralPath (Join-Path $repo $file) -Destination $stage }
    $licenses = Join-Path $stage 'licenses'
    New-Item -ItemType Directory -Force $licenses | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'vendor/zwo/LICENSE.txt') -Destination (Join-Path $licenses 'ZWO-ASI-SDK.txt')
    $metadata = cargo metadata --locked --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE) { throw 'Cargo metadata failed' }
    foreach ($package in $metadata.packages) {
        if ($null -eq $package.source) { continue }
        $dir = Split-Path $package.manifest_path
        $texts = @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE|COPYRIGHT)' })
        if ($texts.Count -eq 0) { throw "Missing license text: $($package.name)" }
        $dest = Join-Path $licenses "rust/$($package.name)-$($package.version)"
        New-Item -ItemType Directory -Force $dest | Out-Null
        foreach ($text in $texts) { Copy-Item -LiteralPath $text.FullName -Destination $dest }
    }
    $rustRoot = rustc --print sysroot
    Copy-Item -LiteralPath (Join-Path $rustRoot 'share/doc/rust/COPYRIGHT-library.html') -Destination (Join-Path $licenses 'Rust-Standard-Library.html')
    $archive = Join-Path $out 'ZwoGain-0.1.0.0.zip'
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive -Force
    $checksum = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    "$checksum  ZwoGain-0.1.0.0.zip" | Set-Content -LiteralPath (Join-Path $out 'SHA256SUMS')
    if ($Install) {
        $destination = Join-Path $env:LOCALAPPDATA 'NINA/Plugins/3.0.0/ZwoGain'
        New-Item -ItemType Directory -Force $destination | Out-Null
        Get-ChildItem -LiteralPath $stage | Copy-Item -Destination $destination -Recurse -Force
        Write-Output "Installed to $destination. Restart NINA to load it."
    }
    Write-Output "Package: $archive"
    Write-Output "SHA256: $checksum"
} finally { Pop-Location }
