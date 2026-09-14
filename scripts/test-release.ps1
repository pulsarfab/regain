# Exercise the real registry script against local fixtures; no HTTP or remote push.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$version = & (Join-Path $PSScriptRoot 'version.ps1')
$manifestSource = Join-Path $repo "artifacts/ZwoGain-$version.manifest.json"
$archiveSource = Join-Path $repo "artifacts/ZwoGain-$version.zip"
if (!(Test-Path -LiteralPath $manifestSource)) { throw 'Run build.ps1 first.' }
$root = Join-Path $repo ('artifacts/release-tests-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
$global:ZwoGainReleaseFixture = @{ Manifest = (Get-Content $manifestSource -Raw); PrivateAsset = $false; Draft = $false }
function gh {
    $global:LASTEXITCODE = 0
    if ($args[0] -eq 'release' -and $args[1] -eq 'view') {
        return (@{ isDraft = $global:ZwoGainReleaseFixture.Draft; isPrerelease = $false; tagName = "v$version"; assets = @(@{name="ZwoGain-$version.manifest.json"}) } | ConvertTo-Json -Depth 5)
    }
    if ($args[0] -eq 'release' -and $args[1] -eq 'download') {
        $dirIndex = [array]::IndexOf($args, '--dir')
        if ($dirIndex -lt 0) { throw 'Missing fixture output directory.' }
        [IO.File]::WriteAllText((Join-Path $args[$dirIndex+1] "ZwoGain-$version.manifest.json"), $global:ZwoGainReleaseFixture.Manifest)
        return
    }
    throw "Unexpected gh invocation: $args"
}
function Invoke-WebRequest {
    param($Uri, $OutFile, $TimeoutSec)
    if ($global:ZwoGainReleaseFixture.PrivateAsset) { throw 'Fixture: anonymous download returned 404.' }
    if ($Uri.EndsWith('.zip')) { Copy-Item -LiteralPath $archiveSource -Destination $OutFile }
    elseif ($Uri.EndsWith('/zwogain.png')) { Copy-Item -LiteralPath (Join-Path $repo 'artifacts/zwogain.png') -Destination $OutFile }
    else { throw "Unexpected download URL: $Uri" }
}
function New-Fixture([string]$name) {
    $path = Join-Path $root $name
    git init --quiet --initial-branch=main $path
    if ($LASTEXITCODE) { throw 'Fixture git init failed.' }
    git -C $path remote add origin https://github.com/theatrus/nina-plugins-registry.git
    if ($LASTEXITCODE) { throw 'Fixture remote setup failed.' }
    return $path
}
function Assert-Rejected([string]$name, [string]$message) {
    $path = New-Fixture $name
    $rejected = $false
    try { & (Join-Path $PSScriptRoot 'publish-registry.ps1') -Tag "v$version" -RegistryPath $path }
    catch { if ($_.Exception.Message -notlike "*$message*") { throw }; $rejected = $true }
    if (!$rejected) { throw "$name should have been rejected." }
    if (Test-Path -LiteralPath (Join-Path $path 'manifests')) { throw "$name modified the registry before validating assets." }
    Write-Output "PASS: $name"
}
$good = New-Fixture 'valid'
& (Join-Path $PSScriptRoot 'publish-registry.ps1') -Tag "v$version" -RegistryPath $good
$destination = Join-Path $good 'manifests/z/ZwoGain/3.2.0.9001/manifest.json'
if ((Get-Content $destination -Raw) -cne $global:ZwoGainReleaseFixture.Manifest) { throw 'Registry metadata changed during publication.' }
Write-Output 'PASS: valid public package and logo prepare the exact manifest'
$global:ZwoGainReleaseFixture.PrivateAsset = $true
Assert-Rejected 'private-assets' 'anonymous download'
$global:ZwoGainReleaseFixture.PrivateAsset = $false
$global:ZwoGainReleaseFixture.Draft = $true
Assert-Rejected 'draft-release' 'published stable'
$global:ZwoGainReleaseFixture.Draft = $false
$tampered = $global:ZwoGainReleaseFixture.Manifest | ConvertFrom-Json
$tampered.Installer.Checksum = '0' * 64
$global:ZwoGainReleaseFixture.Manifest = $tampered | ConvertTo-Json -Depth 10
Assert-Rejected 'checksum-mismatch' 'checksum mismatch'
$global:ZwoGainReleaseFixture.Manifest = Get-Content $manifestSource -Raw
$tampered = $global:ZwoGainReleaseFixture.Manifest | ConvertFrom-Json
$tampered.Version.Build = '65534'
$global:ZwoGainReleaseFixture.Manifest = $tampered | ConvertTo-Json -Depth 10
Assert-Rejected 'version-mismatch' 'release version'
Write-Output 'Release publication checks passed without remote writes.'
Remove-Variable ZwoGainReleaseFixture -Scope Global
