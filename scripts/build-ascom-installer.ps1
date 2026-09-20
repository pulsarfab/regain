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
foreach ($file in 'ZwoGain.ASCOM.Register.exe','ZwoGain.FocusCube.ASCOM.exe','ZwoGain.ASCOM.dll','ZwoGain.Rotator.dll','zwogain-caa.exe','zwogain-accessories.exe','zwogain-fc3.exe','zwogain-camera.exe','zwogain-alpaca.exe','zwogain-host.exe','zwogain-direct.exe','ASICamera2.dll','LICENSE') {
    if (!(Test-Path -LiteralPath (Join-Path $stage $file))) { throw "Missing $file. Run scripts/build-ascom.ps1 first." }
}
$assembly = [Reflection.AssemblyName]::GetAssemblyName((Join-Path $stage 'ZwoGain.ASCOM.dll'))
$registry = [Collections.Generic.List[string]]::new()
# These are the CLR activation entries emitted by RegAsm /regfile, plus the
# Chooser entries. Inno owns them so registry failures roll back with the files.
$devices = @(1..4 | ForEach-Object {
    @{ Class = "ZwoGain.Ascom.Camera$_"; ProgId = "ASCOM.ZWOgain.Camera$_";
       Clsid = "{{D1DB6F94-5CC0-4752-A758-F849098874A$_}"; Type = 'Camera'; Name = "ZWOgain Retryable Camera $_" }
}) + @(@{ Class = 'ZwoGain.Ascom.CaaRotator'; ProgId = 'ASCOM.ZWOgain.Rotator';
    Clsid = '{{A918164B-49DD-4FF5-BEE6-A4AB93B97F12}'; Type = 'Rotator'; Name = 'ZWOgain CAA Rotator' },
    @{ Class = 'ZwoGain.Ascom.EfwFilterWheel'; ProgId = 'ASCOM.ZWOgain.FilterWheel'; Clsid = '{{EA2040E1-E936-4BDF-87F7-B58CA3E418AB}'; Type = 'FilterWheel'; Name = 'ZWOgain EFW Filter Wheel' },
    @{ Class = 'ZwoGain.Ascom.EafFocuser'; ProgId = 'ASCOM.ZWOgain.Focuser'; Clsid = '{{295C08F8-EDE9-43C5-9D55-627A063D74CA}'; Type = 'Focuser'; Name = 'ZWOgain EAF Focuser' })
foreach ($root in 'HKLM32','HKLM64') {
    foreach ($device in $devices) {
        $class = $device.Class
        $progId = $device.ProgId
        $clsid = $device.Clsid
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
            @("Software\ASCOM\$($device.Type) Drivers\$progId", '', $device.Name, 'uninsdeletekey')
        )
        foreach ($row in $rows) {
            $registry.Add(('Root: {0}; Subkey: "{1}"; ValueType: string; ValueName: "{2}"; ValueData: "{3}"; Flags: {4}' -f $root, $row[0], $row[1], $row[2], $row[3]))
        }
        $registry.Add(('Root: {0}; Subkey: "{1}\Implemented Categories\{{{{62C8FE65-4EBB-45E7-B440-6E39B2CDBF29}}"; ValueType: none' -f $root, $key))
    }
    # FocusCube3 is a real COM local server, not a RegAsm in-process class.
    $fc3 = 'Software\Classes\CLSID\{{69AB224B-14D2-46A2-A744-0C60593A28B3}'
    foreach ($row in @(
        @($fc3, 'ZWOgain Pegasus FocusCube3', 'uninsdeletekey'),
        @("$fc3\LocalServer32", '"{app}\ZwoGain.FocusCube.ASCOM.exe" /Embedding', ''),
        @("$fc3\ProgID", 'ASCOM.ZWOgain.FocusCube3.Focuser', ''),
        @('Software\Classes\ASCOM.ZWOgain.FocusCube3.Focuser', 'ZWOgain Pegasus FocusCube3', 'uninsdeletekey'),
        @('Software\Classes\ASCOM.ZWOgain.FocusCube3.Focuser\CLSID', '{{69AB224B-14D2-46A2-A744-0C60593A28B3}', ''),
        @('Software\ASCOM\Focuser Drivers\ASCOM.ZWOgain.FocusCube3.Focuser', 'ZWOgain Pegasus FocusCube3', 'uninsdeletekey')
    )) {
        $registry.Add(('Root: {0}; Subkey: "{1}"; ValueType: string; ValueName: ""; ValueData: "{2}"; Flags: {3}' -f $root, $row[0], $row[1].Replace('"','""'), $row[2]))
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
