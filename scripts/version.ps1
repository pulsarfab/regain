[CmdletBinding()]
param([string]$Tag)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
[xml]$props = Get-Content -LiteralPath (Join-Path $repo 'Directory.Build.props') -Raw
$version = [string]$props.Project.PropertyGroup.Version
if ($version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') { throw 'Use a four-part NINA version in Directory.Build.props.' }
if ($version.Split('.') | Where-Object { [long]$_ -gt 65534 }) { throw 'Assembly version components must fit 16 bits.' }
$cargo = Get-Content -LiteralPath (Join-Path $repo 'Cargo.toml') -Raw
if ($cargo -notmatch '(?m)^version\s*=\s*"([^"]+)"') { throw 'Cargo workspace version missing.' }
if ($Matches[1] -ne ($version.Split('.')[0..2] -join '.')) { throw 'Rust version must match the first three NINA version components.' }
if ($Tag -and $Tag -cne "v$version") { throw "Tag $Tag does not match source version v$version." }
$version
