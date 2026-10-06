# Fixture-only loader diagnostics. Never accepts an installed driver ProgID.
$ErrorActionPreference = 'Stop'
if ($env:REGAIN_HUB_COM_FIXTURE_PROGID -notmatch '^ASCOM\.Regain\.HubFixture\.[a-f0-9]{32}$') { throw 'Not a private COM fixture' }
if (!(Test-Path -LiteralPath $env:REGAIN_COM_FIXTURE_DLL -PathType Leaf)) { throw 'Missing fixture assembly' }
Write-Output "Fixture probe: bitness=$([IntPtr]::Size * 8) elevated=$(([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator))"
Get-ChildItem -LiteralPath (Split-Path $env:REGAIN_COM_FIXTURE_DLL) -Filter '*.dll' | ForEach-Object { Write-Output "Fixture DLL: $($_.Name)" }
foreach ($key in @("Registry::HKEY_CLASSES_ROOT\$env:REGAIN_HUB_COM_FIXTURE_PROGID\CLSID", 'Registry::HKEY_CLASSES_ROOT\CLSID\{E86CDFE1-0282-4A40-9265-F7D6A2CEBAF1}\InprocServer32')) {
    if (Test-Path -LiteralPath $key) { Get-ItemProperty -LiteralPath $key | Format-List | Out-String | Write-Output }
    else { Write-Output "Missing merged registration: $key" }
}
function Report-Error($failure) {
    $cause = $failure.Exception
    while ($null -ne $cause) {
        Write-Output "Fixture exception: $($cause.GetType().FullName) HRESULT=$($cause.HResult) message=$($cause.Message)"
        if ($cause -is [IO.FileNotFoundException] -or $cause -is [IO.FileLoadException]) {
            Write-Output "Fixture missing/failed file: $($cause.FileName) fusion=$($cause.FusionLog)"
        }
        $cause = $cause.InnerException
    }
}
try {
    $type = [Type]::GetTypeFromProgID($env:REGAIN_HUB_COM_FIXTURE_PROGID, $true)
    $driver = [Activator]::CreateInstance($type)
    Write-Output 'Fixture COM activation passed'
    if ([Runtime.InteropServices.Marshal]::IsComObject($driver)) { [Runtime.InteropServices.Marshal]::FinalReleaseComObject($driver) | Out-Null }
} catch { Report-Error $_ }
try {
    $assembly = [Reflection.Assembly]::LoadFrom($env:REGAIN_COM_FIXTURE_DLL)
    $driver = $assembly.CreateInstance('Regain.Hub.COM.Fixture.Driver')
    if ($null -eq $driver) { throw 'Missing fixture class' }
    Write-Output 'Fixture direct managed load passed'
} catch { Report-Error $_ }
