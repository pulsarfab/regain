[CmdletBinding()]
param([string]$Compiler, [switch]$Signed)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$version = & (Join-Path $PSScriptRoot 'version.ps1')
if (!$Compiler) {
    $Compiler = @((Join-Path $repo 'artifacts/tools/inno/ISCC.exe'), "${env:ProgramFiles(x86)}/Inno Setup 6/ISCC.exe", "$env:ProgramFiles/Inno Setup 6/ISCC.exe") | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (!$Compiler) { throw 'Install Inno Setup 6.7 or later, or pass -Compiler with the ISCC.exe path.' }
$stage = Join-Path $repo 'artifacts/ascom-stage'
foreach ($file in 'ZwoGain.ASCOM.Register.exe','ZwoGain.ASCOM.dll','zwogain-alpaca.exe','zwogain-host.exe','zwogain-direct.exe','ASICamera2.dll','LICENSE') {
    if (!(Test-Path -LiteralPath (Join-Path $stage $file))) { throw "Missing $file. Run scripts/build-ascom.ps1 first." }
}
$arguments = @('/Qp', "/DAppVersion=$version", "/DStage=$stage", "/DOutput=$repo/artifacts")
if ($Signed) {
    $signScript = Join-Path $PSScriptRoot 'sign-installer-file.ps1'
    $arguments += '/DSignedBuild'
    $arguments += ('/Sazure=pwsh.exe -NoProfile -File $q' + $signScript + '$q -Path $f')
}
& $Compiler @arguments (Join-Path $repo 'installer/ascom.iss')
if ($LASTEXITCODE) { throw 'ASCOM installer compilation failed' }
$installer = Join-Path $repo "artifacts/ZwoGain-ASCOM-$version-win-x64-setup.exe"
if ($Signed) {
    $signature = Get-AuthenticodeSignature -LiteralPath $installer
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'StackFoundry LLC') { throw 'Invalid installer signature' }
}
((Get-FileHash -LiteralPath $installer).Hash + '  ' + [IO.Path]::GetFileName($installer)) | Set-Content -LiteralPath "$installer.sha256"
Write-Output "Installer: $installer"
