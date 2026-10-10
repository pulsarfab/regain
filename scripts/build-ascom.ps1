[CmdletBinding()]
param([switch]$StageOnly, [switch]$PackageOnly)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$stage = Join-Path $repo 'artifacts/ascom-stage'
$plugin = Join-Path $repo 'artifacts/stage'
$version = & (Join-Path $PSScriptRoot 'version.ps1')
Push-Location $repo
try {
    if (!(Test-Path -LiteralPath (Join-Path $plugin 'regain-alpaca.exe'))) { throw 'Run scripts/build.ps1 -StageOnly first.' }
    if (!$PackageOnly) {
        dotnet build src/Regain.ASCOM.Register -c Release
        if ($LASTEXITCODE) { throw 'ASCOM build failed' }
        # Both paths are fixed children of this repository's artifacts directory.
        if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
        New-Item -ItemType Directory -Path $stage | Out-Null
        Get-ChildItem -LiteralPath (Join-Path $repo 'src/Regain.ASCOM.Register/bin/Release/net48') -File | Where-Object { $_.Extension -in '.dll','.exe','.config' } | Copy-Item -Destination $stage
        foreach ($file in 'ASICamera2.dll','LICENSE','THIRD_PARTY_NOTICES.md','caa.md','caa-frontends.md') { Copy-Item -LiteralPath (Join-Path $plugin $file) -Destination $stage }
        Copy-Item -LiteralPath (Join-Path $plugin 'licenses') -Destination $stage -Recurse
        Copy-Item -LiteralPath (Join-Path $repo 'docs/ascom.md') -Destination (Join-Path $stage 'README.md')
        Copy-Item -LiteralPath (Join-Path $repo 'docs/accessories.md') -Destination $stage
        Copy-Item -LiteralPath (Join-Path $repo 'docs/ofp2.md') -Destination $stage
        foreach ($file in 'asi662mc.md','asi662-validation.json','asi585mm-pro.md','asi585-validation.json','usb-recovery.md','focusers.md','falcon-v2.md','falcon-v2-evidence.json','falcon-v2-serial.jsonl','focuscube3.md','focuscube3-evidence.json','focuscube3-serial.jsonl','eta.md','eta-evidence.json','eta-serial.jsonl') { Copy-Item -LiteralPath (Join-Path $repo ('docs/' + $file)) -Destination $stage }
        Copy-Item -LiteralPath (Join-Path $repo 'docs/ofp2-evidence.json') -Destination $stage
        Copy-Item -LiteralPath (Join-Path $repo 'docs/accessory-evidence.json') -Destination $stage
        Copy-Item -LiteralPath (Join-Path $repo 'docs/images') -Destination $stage -Recurse
        & (Join-Path $PSScriptRoot 'stage-dotnet-licenses.ps1') -ProjectAssets (Join-Path $repo 'src/Regain.ASCOM/obj/project.assets.json') -Destination $stage
    }
    # On releases these are copied after signing the shared Rust payload.
    foreach ($file in 'regain-camera.exe','regain-alpaca.exe','regain-device.exe') { Copy-Item -LiteralPath (Join-Path $plugin $file) -Destination $stage -Force }
    # Refresh the complete private worker tree after release signing, including
    # architecture-specific JSON/runtime dependencies and license texts.
    Copy-Item -LiteralPath (Join-Path $plugin 'hub-ascom') -Destination $stage -Recurse -Force
    python scripts/check-windows-runtime.py $stage
    if ($LASTEXITCODE) { throw 'ASCOM payload requires an unbundled VC++ runtime' }
    if ($StageOnly) { Write-Output "Staged: $stage"; return }
    $archive = Join-Path $repo "artifacts/Regain-ASCOM-$version-win-x64.zip"
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive -Force
    ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash + '  ' + [IO.Path]::GetFileName($archive)) | Set-Content -LiteralPath "$archive.sha256"
    Write-Output "Package: $archive"
} finally { Pop-Location }
