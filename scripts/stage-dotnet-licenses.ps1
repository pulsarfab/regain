[CmdletBinding()]
param([Parameter(Mandatory)][string]$ProjectAssets, [Parameter(Mandatory)][string]$Destination)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$assets = Get-Content -LiteralPath $ProjectAssets -Raw | ConvertFrom-Json -AsHashtable
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
    if ($lib.type -eq 'project') { continue }
    $dir = Join-Path @($assets.packageFolders.Keys)[0] $lib.path
    $specFile = Get-ChildItem -LiteralPath $dir -Filter '*.nuspec' | Select-Object -First 1
    $spec = [xml](Get-Content -LiteralPath $specFile.FullName -Raw)
    if ($spec.package.metadata.license.'#text' -ne 'MIT') { throw "Review license for $($entry.Key)" }
    $licenseDir = Join-Path $Destination ('licenses/dotnet/' + $entry.Key.Replace('/', '-'))
    New-Item -ItemType Directory -Path $licenseDir -Force | Out-Null
    ($spec.package.metadata.copyright + "`n`n" + $mit) | Set-Content -LiteralPath (Join-Path $licenseDir 'LICENSE.txt')
    Copy-Item -LiteralPath $specFile.FullName -Destination $licenseDir
    Get-ChildItem -LiteralPath $dir -File | Where-Object { $_.Name -match '^(LICENSE|NOTICE|COPYRIGHT|THIRD.PARTY)' } | Copy-Item -Destination $licenseDir
}
