param([int]$Slot, [switch]$MetadataOnly)
$ErrorActionPreference = 'Stop'
# Exercise .NET Framework's redirected-stdin UTF-8 preamble, as on Windows CI.
[Console]::InputEncoding = [Text.UTF8Encoding]::new($true)
$id = [Guid](('D1DB6F94-5CC0-4752-A758-F849098874A{0}' -f ($Slot + 1)))
if ($env:REGAIN_ASCOM_TEST_CLSIDS) { $id = [Guid]($env:REGAIN_ASCOM_TEST_CLSIDS.Split(',')[$Slot]) }
$deadline = [DateTime]::UtcNow.AddSeconds(20)
do {
    try { $camera = [Activator]::CreateInstance([Type]::GetTypeFromCLSID($id)); break }
    catch { if (!$env:REGAIN_ASCOM_TEST_CLSIDS -or [DateTime]::UtcNow -gt $deadline) { throw }; Start-Sleep -Milliseconds 100 }
} while ($true)
try {
    if ($camera.InterfaceVersion -ne 4) { throw 'Wrong camera interface' }
    if ($MetadataOnly) { Write-Output $camera.Name; return }
    $camera.Connected = $true
    $camera.BinX = 1
    $camera.BinY = 1
    $camera.NumX = 64
    $camera.NumY = 64
    $camera.Gain = 100
    $camera.StartExposure(0.01, $false)
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (!$camera.ImageReady) {
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Capture timed out' }
        Start-Sleep -Milliseconds 20
    }
    $pixels = $camera.ImageArray
    if ($pixels.Rank -ne 2 -or $pixels.GetLength(0) -ne 64 -or $pixels.GetLength(1) -ne 64) { throw 'Wrong native image dimensions' }
    if ($camera.LastExposureStartTime -notmatch '^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d$') { throw 'Invalid exposure timestamp' }
    $variant = $camera.ImageArrayVariant
    if ($variant.GetLength(0) -ne 64 -or $variant[3,5] -ne $pixels[3,5]) { throw 'Variant image mismatch' }
    if ($camera.DriverInfo -match 'Alpaca') { throw 'COM still uses the Alpaca bridge' }
    $camera.StartExposure(2.0, $false)
    $camera.AbortExposure()
    if ($camera.CameraState -ne 0 -or $camera.ImageReady) { throw 'Abort did not return idle' }
    $camera.Disconnect()
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ($camera.Connecting) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Disconnect timed out' }; Start-Sleep -Milliseconds 20 }
    if ($camera.Connected) { throw 'Still connected' }
    $camera.Connect()
    while ($camera.Connecting) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Connect timed out' }; Start-Sleep -Milliseconds 20 }
    if (!$camera.Connected) { throw 'Not connected' }
    Write-Output ("{0}-bit COM slot {1}: native capture, variant image, async connection and abort passed" -f ([IntPtr]::Size * 8), $Slot)
} finally {
    try { $camera.Connected = $false } finally {
        if ([Runtime.InteropServices.Marshal]::IsComObject($camera)) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($camera) }
        else { $camera.Dispose() } # CLR unwraps an in-process managed COM object.
    }
}
