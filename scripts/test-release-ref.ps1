# Real local Git history; no network, signing, registry writes, or installed tools.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$fixture = Join-Path $repo ('artifacts/release-ref-tests-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $fixture 'scripts') -Force | Out-Null
Copy-Item (Join-Path $PSScriptRoot 'version.ps1'), (Join-Path $PSScriptRoot 'check-release-ref.ps1') (Join-Path $fixture 'scripts')
$check = Join-Path $fixture 'scripts/check-release-ref.ps1'
function Invoke-FixtureGit {
    & git -C $fixture @args
    if ($LASTEXITCODE) { throw "Fixture git command failed: $args" }
}
function Set-Version([string]$value) {
    [IO.File]::WriteAllText((Join-Path $fixture 'Directory.Build.props'), "<Project><PropertyGroup><Version>$value</Version></PropertyGroup></Project>")
    $rust = $value.Split('.')[0..2] -join '.'
    [IO.File]::WriteAllText((Join-Path $fixture 'Cargo.toml'), "[workspace.package]`nversion = `"$rust`"`n[workspace.dependencies]`nregain-core = { path = `"crates/regain-core`", version = `"=$rust`" }`n")
}
function Accept([string]$name, [hashtable]$parameters, [string]$expected) {
    $actual = & $check @parameters
    if ($actual -cne $expected) { throw "$name returned $actual instead of $expected" }
    Write-Output "PASS: $name"
}
function Reject([string]$name, [hashtable]$parameters, [string]$message) {
    $rejected = $false
    try { & $check @parameters | Out-Null }
    catch {
        if ($_.Exception.Message -notlike "*$message*") { throw }
        $rejected = $true
    }
    if (!$rejected) { throw "$name should have been rejected" }
    Write-Output "PASS: $name"
}
Invoke-FixtureGit init --quiet --initial-branch=main
Invoke-FixtureGit config user.name 'Release fixture'
Invoke-FixtureGit config user.email 'fixture@example.invalid'
Invoke-FixtureGit config commit.gpgsign false
Invoke-FixtureGit config tag.gpgsign false
Set-Version '0.5.10.0'
Invoke-FixtureGit add .
Invoke-FixtureGit commit --quiet -m 'Current release'
Invoke-FixtureGit tag v0.5.10.0
Invoke-FixtureGit update-ref refs/remotes/origin/main HEAD
Invoke-FixtureGit update-ref refs/remotes/origin/codex/feature HEAD
Invoke-FixtureGit update-ref refs/remotes/origin/release/0.5 HEAD
Accept 'main build' @{RefType='branch'; RefName='main'} '0.5.10.0'
Accept 'maintenance build' @{RefType='branch'; RefName='release/0.5'} '0.5.10.0'
$manifest = Join-Path $fixture 'Cargo.toml'
$validManifest = [IO.File]::ReadAllText($manifest)
[IO.File]::WriteAllText($manifest, $validManifest.Replace('=0.5.10', '0.5.10'))
Reject 'unbounded internal crate dependency' @{RefType='branch'; RefName='main'} 'must pin version =0.5.10'
[IO.File]::WriteAllText($manifest, $validManifest.Replace('=0.5.10', '=0.5.9'))
Reject 'mismatched internal crate dependency' @{RefType='branch'; RefName='main'} 'must pin version =0.5.10'
[IO.File]::WriteAllText($manifest, $validManifest)
Accept 'maintenance tag' @{RefType='tag'; RefName='v0.5.10.0'} '0.5.10.0'
Accept 'maintenance registry publication' @{RefType='branch'; RefName='release/0.5'; RegistryTag='v0.5.10.0'} '0.5.10.0'
Reject 'feature branch' @{RefType='branch'; RefName='codex/feature'} 'require main or'
Accept 'signed candidate on a feature branch' @{RefType='branch'; RefName='codex/feature'; Candidate=$true} '0.5.10.0'
Reject 'candidate tag' @{RefType='tag'; RefName='v0.5.10.0'; Candidate=$true} 'require a branch'
Reject 'candidate registry publication' @{RefType='branch'; RefName='codex/feature'; Candidate=$true; RegistryTag='v0.5.10.0'} 'cannot publish'
Reject 'missing candidate branch' @{RefType='branch'; RefName='codex/missing'; Candidate=$true} 'branch is missing'
Reject 'malformed maintenance branch' @{RefType='branch'; RefName='release/0.5/feature'} 'require main or'
Reject 'wrong maintenance train' @{RefType='branch'; RefName='release/0.6'} 'does not belong'
Reject 'wrong source version' @{RefType='tag'; RefName='v0.5.11.0'} 'does not match source'
Reject 'missing registry tag' @{RefType='branch'; RefName='release/0.5'; RegistryTag='v0.5.99.0'} 'commit is missing'
Reject 'malformed registry tag' @{RefType='branch'; RefName='main'; RegistryTag='--help'} 'stable four-part tag'
Reject 'registry dispatched on tag' @{RefType='tag'; RefName='v0.5.10.0'; RegistryTag='v0.5.10.0'} 'requires a branch'

# Main has moved ahead, but a maintenance tag must remain on its release branch.
Invoke-FixtureGit commit --quiet --allow-empty -m 'Main-only change'
Reject 'candidate commit outside remote branch' @{RefType='branch'; RefName='codex/feature'; Candidate=$true} 'not contained'
Invoke-FixtureGit update-ref refs/remotes/origin/main HEAD
Invoke-FixtureGit tag v0.5.11.0
Reject 'main-only maintenance release' @{RefType='branch'; RefName='release/0.5'; RegistryTag='v0.5.11.0'} 'not contained'
Reject 'main-only maintenance tag build' @{RefType='tag'; RefName='v0.5.10.0'} 'not contained'
Set-Version '0.6.0.0'
Invoke-FixtureGit add .
Invoke-FixtureGit commit --quiet -m 'Next development train'
Invoke-FixtureGit update-ref refs/remotes/origin/main HEAD
Invoke-FixtureGit tag v0.6.0.0
Accept 'next train main build' @{RefType='branch'; RefName='main'} '0.6.0.0'
Accept 'new train tag falls back to main' @{RefType='tag'; RefName='v0.6.0.0'} '0.6.0.0'
Accept 'old tag publication from newer main' @{RefType='branch'; RefName='main'; RegistryTag='v0.5.10.0'} '0.5.10.0'
Reject 'cross-train registry publication' @{RefType='branch'; RefName='release/0.5'; RegistryTag='v0.6.0.0'} 'does not belong'

# A maintenance-only patch can be released without first merging it into main.
Invoke-FixtureGit checkout --quiet --detach refs/remotes/origin/release/0.5
Set-Version '0.5.12.0'
Invoke-FixtureGit add .
Invoke-FixtureGit commit --quiet -m 'Maintenance-only patch'
Invoke-FixtureGit update-ref refs/remotes/origin/release/0.5 HEAD
Invoke-FixtureGit tag v0.5.12.0
Accept 'maintenance-only tag build' @{RefType='tag'; RefName='v0.5.12.0'} '0.5.12.0'
Accept 'maintenance-only registry publication' @{RefType='branch'; RefName='release/0.5'; RegistryTag='v0.5.12.0'} '0.5.12.0'
Reject 'unmerged tag from main' @{RefType='branch'; RefName='main'; RegistryTag='v0.5.12.0'} 'not contained'
$global:LASTEXITCODE = 0 # Expected Git failures above must not fail the CI shell.
Write-Output 'Release ref validation passed without remote writes.'
