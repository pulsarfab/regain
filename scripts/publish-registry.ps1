[CmdletBinding()]
param(
    [Parameter(Mandatory)][ValidatePattern('^v\d+\.\d+\.\d+\.\d+$')][string]$Tag,
    [Parameter(Mandatory)][string]$RegistryPath,
    [switch]$Push
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$version = $Tag.Substring(1)
$registry = [IO.Path]::GetFullPath($RegistryPath)
if (!(Test-Path -LiteralPath (Join-Path $registry '.git'))) { throw 'RegistryPath must be a git checkout.' }
$remote = git -C $registry remote get-url origin
if ($LASTEXITCODE -or $remote -notmatch '^(https://github\.com/|git@github\.com:)theatrus/nina-plugins-registry(\.git)?$') { throw 'Expected theatrus/nina-plugins-registry origin.' }
$dirty = git -C $registry status --porcelain
if ($LASTEXITCODE -or $dirty) { throw 'Use a clean registry checkout.' }
$branch = git -C $registry branch --show-current
if ($LASTEXITCODE -or $branch -ne 'main') { throw 'Registry checkout must be on main.' }

$releaseJson = gh release view $Tag --repo pulsarfab/regain --json isDraft,isPrerelease,tagName,assets
if ($LASTEXITCODE) { throw 'Release lookup failed.' }
$release = $releaseJson | ConvertFrom-Json
if ($release.isDraft -or $release.isPrerelease -or $release.tagName -cne $Tag) { throw 'Only published stable releases can enter this registry.' }
$manifestName = "Regain-$version.manifest.json"
if (@($release.assets | Where-Object name -eq $manifestName).Count -ne 1) { throw 'Release manifest is missing or ambiguous.' }
$temp = Join-Path ([IO.Path]::GetTempPath()) ('regain-registry-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $temp | Out-Null
gh release download $Tag --repo pulsarfab/regain --pattern $manifestName --dir $temp
if ($LASTEXITCODE) { throw 'Release manifest download failed.' }
$manifestPath = Join-Path $temp $manifestName
$manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
if ($manifest.Name -cne 'PulsarFab regain' -or $manifest.Identifier -cne '6953efde-7f7e-48df-94d5-671986293974' -or $manifest.License -cne 'Apache-2.0') { throw 'Unexpected plugin identity or license.' }
$actualVersion = '{0}.{1}.{2}.{3}' -f $manifest.Version.Major, $manifest.Version.Minor, $manifest.Version.Patch, $manifest.Version.Build
if ($actualVersion -cne $version) { throw 'Manifest does not match release version.' }
$baseUrl = "https://github.com/pulsarfab/regain/releases/download/$Tag"
if ($manifest.Installer.URL -cne "$baseUrl/Regain-$version.zip" -or $manifest.Descriptions.FeaturedImageURL -cne "$baseUrl/regain.png" -or $manifest.Installer.Type -cne 'ARCHIVE' -or $manifest.Installer.ChecksumType -cne 'SHA256' -or $manifest.Installer.Checksum -notmatch '^[a-fA-F0-9]{64}$') { throw 'Unexpected release asset URLs or checksum format.' }
if ($manifest.PSObject.Properties.Name -contains 'Channel') { throw 'This registry publishes stable manifests without Channel.' }
$minimum = '{0}.{1}.{2}.{3}' -f $manifest.MinimumApplicationVersion.Major, $manifest.MinimumApplicationVersion.Minor, $manifest.MinimumApplicationVersion.Patch, $manifest.MinimumApplicationVersion.Build
if ($minimum -notmatch '^\d+\.\d+\.\d+\.\d+$') { throw 'Invalid minimum NINA version.' }

# No GH_TOKEN or Authorization headers: verify the same access NINA users get.
$archive = Join-Path $temp 'download.zip'
Invoke-WebRequest -Uri $manifest.Installer.URL -OutFile $archive -TimeoutSec 180
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -cne $manifest.Installer.Checksum.ToUpperInvariant()) { throw 'Public archive checksum mismatch.' }
$logo = Join-Path $temp 'regain.png'
Invoke-WebRequest -Uri $manifest.Descriptions.FeaturedImageURL -OutFile $logo -TimeoutSec 60
$png = [IO.File]::ReadAllBytes($logo)
if ($png.Length -lt 24 -or [Convert]::ToHexString($png[0..7]) -cne '89504E470D0A1A0A') { throw 'Public logo is not a PNG.' }
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::OpenRead($archive)
try {
    foreach ($name in @('Regain.NINA.dll','Regain.Core.dll','regain-host.exe','ASICamera2.dll','LICENSE','regain.png')) {
        if ($null -eq $zip.GetEntry($name)) { throw "Public archive missing $name" }
    }
    $entry = $zip.GetEntry('regain.png').Open()
    try { $packagedLogoHash = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($entry)) }
    finally { $entry.Dispose() }
    if ($packagedLogoHash -ne (Get-FileHash -LiteralPath $logo -Algorithm SHA256).Hash) { throw 'Published logo differs from the packaged logo.' }
} finally { $zip.Dispose() }

# Preserve the catalog entry as well as the plugin GUID; a new path would duplicate it.
$relative = "manifests/z/ZwoGain/$minimum/manifest.json"
$destination = Join-Path $registry $relative
if (Test-Path -LiteralPath $destination) {
    $existing = Get-Content -LiteralPath $destination -Raw | ConvertFrom-Json
    $oldVersion = [version]('{0}.{1}.{2}.{3}' -f $existing.Version.Major, $existing.Version.Minor, $existing.Version.Patch, $existing.Version.Build)
    if ($oldVersion -gt [version]$version) { throw 'Refusing to downgrade the registry.' }
    if ($oldVersion -eq [version]$version) {
        if ((Get-FileHash $destination).Hash -eq (Get-FileHash $manifestPath).Hash) { Write-Output 'Registry already has this exact release.'; return }
        throw 'Refusing to replace an existing version with different metadata.'
    }
}
New-Item -ItemType Directory -Path (Split-Path $destination) -Force | Out-Null
Copy-Item -LiteralPath $manifestPath -Destination $destination
if ($Push) {
    git -C $registry add -- $relative
    if ($LASTEXITCODE) { throw 'Could not stage registry manifest.' }
    git -C $registry commit -m "Publish PulsarFab regain $version"
    if ($LASTEXITCODE) { throw 'Could not commit registry manifest.' }
    git -C $registry push origin HEAD:main
    if ($LASTEXITCODE) { throw 'Registry push failed; resolve concurrent changes and retry.' }
}
Write-Output "Verified and prepared $destination"
