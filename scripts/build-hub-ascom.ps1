[CmdletBinding()]
param([string]$Destination = 'target/debug', [switch]$WarningsAsErrors)
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$destinationPath = if ([IO.Path]::IsPathRooted($Destination)) { $Destination } else { Join-Path $repo $Destination }
foreach ($architecture in 'x86', 'x64') {
    $output = Join-Path $destinationPath "hub-ascom/$architecture"
    # SDK intermediate paths omit PlatformTarget: never reuse a differently sized EXE.
    $arguments = @('build', (Join-Path $repo 'src/Regain.Hub.ASCOM'), '-c', 'Release', "-p:PlatformTarget=$architecture", "-p:IntermediateOutputPath=obj/hub-ascom-$architecture/Release/", '-o', $output)
    if ($WarningsAsErrors) { $arguments += '-warnaserror' }
    & dotnet @arguments
    if ($LASTEXITCODE) { throw "Hub ASCOM $architecture build failed" }
}
