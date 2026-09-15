param([int]$Slot, [switch]$MetadataOnly)
$ErrorActionPreference = 'Stop'
$id = [Guid](('D1DB6F94-5CC0-4752-A758-F849098874A{0}' -f ($Slot + 1)))
$camera = [Activator]::CreateInstance([Type]::GetTypeFromCLSID($id))
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
    if ($pixels.Rank -ne 2 -or $pixels.GetLength(0) -ne 64 -or $pixels.GetLength(1) -ne 64) { throw 'Wrong ImageBytes dimensions' }
    if ($camera.LastExposureStartTime -notmatch '^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d$') { throw 'Invalid exposure timestamp' }
    $camera.StartExposure(2.0, $false)
    $camera.AbortExposure()
    if ($camera.CameraState -ne 0 -or $camera.ImageReady) { throw 'Abort did not return idle' }
    Write-Output ("{0}-bit COM slot {1}: capture, ImageBytes and abort passed" -f ([IntPtr]::Size * 8), $Slot)
} finally {
    try { $camera.Connected = $false } finally { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($camera) }
}
