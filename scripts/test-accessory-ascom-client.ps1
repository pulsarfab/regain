param([string]$DeviceClass, [switch]$Hardware, [switch]$MetadataOnly, [switch]$Calibrate)
$ErrorActionPreference = 'Stop'
# Match UTF-8 Windows CI hosts: .NET Framework's redirected stdin writer
# emits this encoding's preamble before the driver's own UTF-8 writer.
[Console]::InputEncoding = [Text.UTF8Encoding]::new($true)
$id = if ($env:ZWOGAIN_ACCESSORY_TEST_CLSID) { [Guid]$env:ZWOGAIN_ACCESSORY_TEST_CLSID } elseif ($DeviceClass -eq 'EfwFilterWheel') { [Guid]'EA2040E1-E936-4BDF-87F7-B58CA3E418AB' } else { [Guid]'295C08F8-EDE9-43C5-9D55-627A063D74CA' }
$device = [Activator]::CreateInstance([Type]::GetTypeFromCLSID($id))
try {
    if ($MetadataOnly) {
        $expected = if ($DeviceClass -eq 'EfwFilterWheel') { 2 } else { 3 }
        if ($device.InterfaceVersion -ne $expected) { throw 'Invalid accessory interface' }
        Write-Output $device.Name; return
    }
    $device.Connected = $true
    $initial = $device.Position
    if ($DeviceClass -eq 'EfwFilterWheel') {
        if ($device.InterfaceVersion -ne 2 -or $device.Names.Count -ne 7 -or $device.FocusOffsets.Count -ne 7) { throw 'Invalid filter wheel metadata' }
        if ($device.SupportedActions -notcontains 'ZwoGain.Calibrate') { throw 'Calibration action missing' }
        if ($Calibrate -or !$Hardware) {
            $started = [DateTime]::UtcNow
            [void]$device.Action('ZwoGain.Calibrate', '')
            $status = $device.Action('ZwoGain.Status', '') | ConvertFrom-Json
            if (!$status.calibrating -or $status.slots -ne 7 -or $device.Position -ne -1) { throw 'Calibration did not report progress' }
            $rejected = $false
            try { [void]$device.Action('ZwoGain.Calibrate', '') } catch { $rejected = $true }
            if (!$rejected) { throw 'Repeated calibration was accepted' }
            while ($device.Position -eq -1) {
                if (([DateTime]::UtcNow - $started).TotalSeconds -gt 95) { throw 'Calibration timed out' }
                Start-Sleep -Milliseconds 100
            }
            if ($device.Position -ne 0 -or $device.Names.Count -ne 7) { throw 'Calibration result mismatch' }
            Write-Output ('EFW calibration passed in {0:N3} seconds' -f ([DateTime]::UtcNow - $started).TotalSeconds)
        }
        $device.Position = [int16](($initial + 1) % 7)
        $deadline = [DateTime]::UtcNow.AddSeconds(30)
        while ($device.Position -eq -1) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Wheel timed out' }; Start-Sleep -Milliseconds 100 }
        if ($device.Position -ne (($initial + 1) % 7)) { throw 'Wheel position mismatch' }
        $device.Position = [int16]$initial
        while ($device.Position -eq -1) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Wheel return timed out' }; Start-Sleep -Milliseconds 100 }
    } else {
        if ($device.SupportedActions -contains 'ZwoGain.Calibrate') { throw 'Focuser advertised EFW calibration' }
        if ($device.InterfaceVersion -ne 3 -or !$device.Absolute -or $device.TempCompAvailable) { throw 'Invalid focuser metadata' }
        $target = if ($initial + 20 -le $device.MaxStep) { $initial + 20 } else { $initial - 20 }
        $device.Move($target)
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        while ($device.IsMoving) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Focuser timed out' }; Start-Sleep -Milliseconds 100 }
        if ($device.Position -ne $target) { throw 'Focuser position mismatch' }
        $device.Move($initial)
        while ($device.IsMoving) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Focuser return timed out' }; Start-Sleep -Milliseconds 100 }
        $device.Halt()
    }
    if ($device.Position -ne $initial) { throw 'Position was not restored' }
    $identity = $device.Action('ZwoGain.Identity', '') | ConvertFrom-Json
    if (!$identity.serial) { throw 'Missing identity' }
    Write-Output ("{0}-bit COM {1}: connected, moved, restored and read identity ({2})" -f ([IntPtr]::Size * 8), $DeviceClass, $(if ($Hardware) { 'USB hardware' } else { 'simulation' }))
} finally {
    try { $device.Connected = $false } finally {
        if ([Runtime.InteropServices.Marshal]::IsComObject($device)) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device) } else { $device.Dispose() }
    }
}
