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
    if (!(Test-Path -LiteralPath (Join-Path $plugin 'zwogain-alpaca.exe'))) { throw 'Run scripts/build.ps1 -StageOnly first.' }
    if (!$PackageOnly) {
        dotnet build src/ZwoGain.ASCOM.Register -c Release
        if ($LASTEXITCODE) { throw 'ASCOM build failed' }
        # Both paths are fixed children of this repository's artifacts directory.
        if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
        New-Item -ItemType Directory -Path $stage | Out-Null
        Get-ChildItem -LiteralPath (Join-Path $repo 'src/ZwoGain.ASCOM.Register/bin/Release/net48') -File | Where-Object { $_.Extension -in '.dll','.exe','.config' } | Copy-Item -Destination $stage
        foreach ($file in 'ASICamera2.dll','LICENSE','THIRD_PARTY_NOTICES.md') { Copy-Item -LiteralPath (Join-Path $plugin $file) -Destination $stage }
        Copy-Item -LiteralPath (Join-Path $plugin 'licenses') -Destination $stage -Recurse
        Copy-Item -LiteralPath (Join-Path $repo 'docs/ascom.md') -Destination (Join-Path $stage 'README.md')
        $assets = Get-Content src/ZwoGain.ASCOM/obj/project.assets.json -Raw | ConvertFrom-Json -AsHashtable
        $mit = @'
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
'@
        foreach ($entry in $assets.targets.Values[0].GetEnumerator()) {
            if (!$entry.Value.ContainsKey('runtime')) { continue }
            $lib = $assets.libraries[$entry.Key]
            $dir = Join-Path @($assets.packageFolders.Keys)[0] $lib.path
            $specFile = Get-ChildItem -LiteralPath $dir -Filter '*.nuspec' | Select-Object -First 1
            $spec = [xml](Get-Content -LiteralPath $specFile.FullName -Raw)
            if ($spec.package.metadata.license.'#text' -ne 'MIT') { throw "Review license for $($entry.Key)" }
            $licenseDir = Join-Path $stage ('licenses/dotnet/' + $entry.Key.Replace('/', '-'))
            New-Item -ItemType Directory -Path $licenseDir -Force | Out-Null
            ($spec.package.metadata.copyright + "`n`n" + $mit) | Set-Content -LiteralPath (Join-Path $licenseDir 'LICENSE.txt')
            Copy-Item -LiteralPath $specFile.FullName -Destination $licenseDir
            Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(LICENSE|NOTICE|COPYRIGHT|THIRD.PARTY)' } | Copy-Item -Destination $licenseDir
        }
    }
    # On releases these are copied after signing the shared Rust payload.
    foreach ($file in 'zwogain-alpaca.exe','zwogain-host.exe','zwogain-direct.exe') { Copy-Item -LiteralPath (Join-Path $plugin $file) -Destination $stage -Force }
    if ($StageOnly) { Write-Output "Staged: $stage"; return }
    $archive = Join-Path $repo "artifacts/ZwoGain-ASCOM-$version-win-x64.zip"
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $archive -Force
    ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash + '  ' + [IO.Path]::GetFileName($archive)) | Set-Content -LiteralPath "$archive.sha256"
    Write-Output "Package: $archive"
} finally { Pop-Location }
