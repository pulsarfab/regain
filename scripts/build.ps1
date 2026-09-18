[CmdletBinding()]
# -StageOnly stops after the payload is staged; -PackageOnly picks up a staged
# payload and archives it. Release signing lives in that gap: the NINA manifest
# pins the archive's SHA-256, so the DLLs and the host exe must be signed before
# Compress-Archive runs, and nothing may touch the stage afterwards. The stage is
# a fixed path so the two halves can find each other across workflow steps.
param([switch]$Install, [switch]$StageOnly, [switch]$PackageOnly)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
Push-Location $repo
try {
    $version = & (Join-Path $PSScriptRoot 'version.ps1')
    $out = Join-Path $repo 'artifacts'
    $stage = Join-Path $out 'stage'
    if ($PackageOnly) {
        if (!(Test-Path -LiteralPath $stage)) { throw 'Run build.ps1 -StageOnly first.' }
    } else {
    cargo build --release --locked
    if ($LASTEXITCODE) { throw 'Rust build failed' }
    dotnet build src/ZwoGain.NINA -c Release
    if ($LASTEXITCODE) { throw 'Plugin build failed' }
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    New-Item -ItemType Directory -Force $stage | Out-Null
    foreach ($file in @('ZwoGain.NINA.dll','ZwoGain.Core.dll','ZwoGain.Rotator.dll')) {
        Copy-Item -LiteralPath (Join-Path $repo "src/ZwoGain.NINA/bin/Release/net8.0-windows7.0/$file") -Destination $stage
    }
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-host.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-direct.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-alpaca.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-camera.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-caa.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'target/release/zwogain-accessories.exe') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'docs/caa.md') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'docs/caa-frontends.md') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'vendor/zwo/ASICamera2.dll') -Destination $stage
    Copy-Item -LiteralPath (Join-Path $repo 'src/ZwoGain.NINA/Assets/zwogain.png') -Destination $stage
    foreach ($file in @('LICENSE','README.md','THIRD_PARTY_NOTICES.md')) { Copy-Item -LiteralPath (Join-Path $repo $file) -Destination $stage }
    Copy-Item -LiteralPath (Join-Path $repo 'docs') -Destination $stage -Recurse
    $licenses = Join-Path $stage 'licenses'
    New-Item -ItemType Directory -Force $licenses | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'vendor/zwo/LICENSE.txt') -Destination (Join-Path $licenses 'ZWO-ASI-SDK.txt')
    Copy-Item -LiteralPath (Join-Path $repo 'crates/zwogain-caa/LICENSE-ZWO') -Destination (Join-Path $licenses 'ZWO-CAA-NTC.txt')
    $target = (rustc -vV | Select-String '^host: ').ToString().Substring(6)
    $metadata = cargo metadata --locked --format-version 1 --filter-platform $target | ConvertFrom-Json
    if ($LASTEXITCODE) { throw 'Cargo metadata failed' }
    $nodes = @{}
    foreach ($node in $metadata.resolve.nodes) { $nodes[$node.id] = $node }
    $resolved = [Collections.Generic.HashSet[string]]::new()
    $pending = [Collections.Generic.Stack[string]]::new()
    foreach ($member in $metadata.workspace_members) { $pending.Push($member) }
    while ($pending.Count) {
        $id = $pending.Pop()
        if ($resolved.Add($id)) { foreach ($dependency in $nodes[$id].deps) { $pending.Push($dependency.pkg) } }
    }
    foreach ($package in $metadata.packages) {
        if ($null -eq $package.source -or !$resolved.Contains($package.id)) { continue }
        $dir = Split-Path $package.manifest_path
        $texts = @(Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE|COPYRIGHT)' })
        if ($texts.Count -eq 0) { throw "Missing license text: $($package.name)" }
        $dest = Join-Path $licenses "rust/$($package.name)-$($package.version)"
        New-Item -ItemType Directory -Force $dest | Out-Null
        foreach ($text in $texts) { Copy-Item -LiteralPath $text.FullName -Destination $dest }
    }
    $rustRoot = rustc --print sysroot
    Copy-Item -LiteralPath (Join-Path $rustRoot 'share/doc/rust/COPYRIGHT-library.html') -Destination (Join-Path $licenses 'Rust-Standard-Library.html')
    if ($StageOnly) { Write-Output "Staged: $stage"; return }
    }
    $archiveName = "ZwoGain-$version.zip"
    $archive = Join-Path $out $archiveName
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive -Force
    $checksum = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    "$checksum  $archiveName" | Set-Content -LiteralPath (Join-Path $out 'SHA256SUMS')
    Copy-Item -LiteralPath (Join-Path $stage 'zwogain.png') -Destination $out
    dotnet run --project tools/ZwoGain.Packaging -c Release -- $archive (Join-Path $out "ZwoGain-$version.manifest.json")
    if ($LASTEXITCODE) { throw 'Package or NINA manifest validation failed' }
    if ($Install) {
        $destination = Join-Path $env:LOCALAPPDATA 'NINA/Plugins/3.0.0/ZwoGain'
        New-Item -ItemType Directory -Force $destination | Out-Null
        Get-ChildItem -LiteralPath $stage | Copy-Item -Destination $destination -Recurse -Force
        Write-Output "Installed to $destination. Restart NINA to load it."
    }
    Write-Output "Package: $archive"
    Write-Output "SHA256: $checksum"
} finally { Pop-Location }
