# Validate a release build or registry publication against its maintenance train.
# Requires a full checkout with origin branch refs and tags (fetch-depth: 0).
[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidateSet('branch', 'tag')][string]$RefType,
    [Parameter(Mandatory)][string]$RefName,
    [string]$RegistryTag,
    [switch]$Candidate
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$tagPattern = '^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$'
if ($Candidate -and ($RegistryTag -or $RefType -ne 'branch')) {
    throw 'Candidate builds require a branch and cannot publish a registry tag.'
}
if ($RegistryTag) {
    if ($RefType -ne 'branch' -or $RegistryTag -cnotmatch $tagPattern) {
        throw 'Registry publication requires a branch and a stable four-part tag.'
    }
    $version = $RegistryTag.Substring(1)
    $target = "refs/tags/$RegistryTag"
} else {
    $version = if ($RefType -eq 'tag') {
        & (Join-Path $PSScriptRoot 'version.ps1') -Tag $RefName
    } else {
        & (Join-Path $PSScriptRoot 'version.ps1')
    }
    $target = 'HEAD'
}
$train = $version.Split('.')[0..1] -join '.'
if ($RefType -eq 'branch') {
    if (!$Candidate -and $RefName -cne 'main' -and $RefName -cnotmatch '^release/(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
        throw 'Release jobs require main or a release/MAJOR.MINOR branch.'
    }
    if (!$Candidate -and $RefName -cne 'main' -and $RefName -cne "release/$train") {
        throw "Version $version does not belong to branch $RefName."
    }
    $branch = "refs/remotes/origin/$RefName"
    git -C $repo check-ref-format $branch
    if ($LASTEXITCODE) { throw 'Invalid candidate branch ref.' }
} else {
    $branch = "refs/remotes/origin/release/$train"
    git -C $repo show-ref --verify --quiet $branch
    if ($LASTEXITCODE -eq 1) { $branch = 'refs/remotes/origin/main' }
    elseif ($LASTEXITCODE -ne 0) { throw 'Could not inspect release branch refs.' }
}
git -C $repo rev-parse --verify --quiet "$target^{commit}" | Out-Null
if ($LASTEXITCODE) { throw "Release commit is missing: $target" }
git -C $repo show-ref --verify --quiet $branch
if ($LASTEXITCODE) { throw "Release branch is missing: $branch (fetch full history)." }
git -C $repo merge-base --is-ancestor $target $branch
if ($LASTEXITCODE) { throw "Release commit $target is not contained in $branch." }
$version
