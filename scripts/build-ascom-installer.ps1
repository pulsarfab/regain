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
$assembly = [Reflection.AssemblyName]::GetAssemblyName((Join-Path $stage 'ZwoGain.ASCOM.dll'))
$registry = [Collections.Generic.List[string]]::new()
# These are the CLR activation entries emitted by RegAsm /regfile, plus the
# Chooser entries. Inno owns them so registry failures roll back with the files.
foreach ($root in 'HKLM32','HKLM64') {
    for ($slot = 1; $slot -le 4; $slot++) {
        $class = "ZwoGain.Ascom.Camera$slot"
        $progId = "ASCOM.ZWOgain.Camera$slot"
        $clsid = "{{D1DB6F94-5CC0-4752-A758-F849098874A$slot}"
        $key = "Software\Classes\CLSID\$clsid"
        $rows = @(
            @("Software\Classes\$progId", '', $class, 'uninsdeletekey'),
            @("Software\Classes\$progId\CLSID", '', $clsid, ''),
            @($key, '', $class, 'uninsdeletekey'),
            @("$key\ProgId", '', $progId, ''),
            @("$key\InprocServer32", '', 'mscoree.dll', ''),
            @("$key\InprocServer32", 'ThreadingModel', 'Both', ''),
            @("$key\InprocServer32", 'Class', $class, ''),
            @("$key\InprocServer32", 'Assembly', $assembly.FullName, ''),
            @("$key\InprocServer32", 'RuntimeVersion', 'v4.0.30319', ''),
            @("$key\InprocServer32", 'CodeBase', '{code:CameraCodeBase}', ''),
            @("$key\InprocServer32\$($assembly.Version)", 'Class', $class, ''),
            @("$key\InprocServer32\$($assembly.Version)", 'Assembly', $assembly.FullName, ''),
            @("$key\InprocServer32\$($assembly.Version)", 'RuntimeVersion', 'v4.0.30319', ''),
            @("$key\InprocServer32\$($assembly.Version)", 'CodeBase', '{code:CameraCodeBase}', ''),
            @("Software\ASCOM\Camera Drivers\$progId", '', "ZWOgain Retryable Camera $slot", 'uninsdeletekey')
        )
        foreach ($row in $rows) {
            $registry.Add(('Root: {0}; Subkey: "{1}"; ValueType: string; ValueName: "{2}"; ValueData: "{3}"; Flags: {4}' -f $root, $row[0], $row[1], $row[2], $row[3]))
        }
        $registry.Add(('Root: {0}; Subkey: "{1}\Implemented Categories\{{{{62C8FE65-4EBB-45E7-B440-6E39B2CDBF29}}"; ValueType: none' -f $root, $key))
    }
}
$registryFile = Join-Path $repo 'artifacts/ascom-installer-registry.iss'
$registry | Set-Content -LiteralPath $registryFile -Encoding utf8
$arguments = @('/Qp', "/DAppVersion=$version", "/DStage=$stage", "/DOutput=$repo/artifacts", "/DRegistryEntries=$registryFile")
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
