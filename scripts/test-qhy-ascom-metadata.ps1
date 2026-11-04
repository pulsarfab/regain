# Registered COM activation only: no device connection and no hardware commands.
$ErrorActionPreference = 'Stop'
foreach ($suffix in '11','12','13','14','20') {
    $clsid = [Guid]("C99D52F5-97D8-4A80-9943-26554DC845" + $suffix)
    $device = [Activator]::CreateInstance([Type]::GetTypeFromCLSID($clsid))
    try {
        if ($device.InterfaceVersion -ne 2 -or $device.Name -notlike '*QHY Filter Wheel*' -or $device.Connected) {
            throw "Invalid QHY wheel metadata: $clsid"
        }
        if ($device.SupportedActions -contains 'Regain.Calibrate') { throw 'Unsupported calibration advertised' }
        Write-Output ("{0}-bit COM: {1}" -f ([IntPtr]::Size * 8), $device.Name)
    } finally {
        if ([Runtime.InteropServices.Marshal]::IsComObject($device)) {
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device)
        } else { $device.Dispose() }
    }
}
