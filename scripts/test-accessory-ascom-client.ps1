param([string]$DeviceClass, [switch]$Hardware, [switch]$MetadataOnly, [switch]$Calibrate,
    [ValidateRange(0,10000)][int]$CalibrationPollDelayMs = 0)
$ErrorActionPreference = 'Stop'
# Match UTF-8 Windows CI hosts: .NET Framework's redirected stdin writer
# emits this encoding's preamble before the driver's own UTF-8 writer.
[Console]::InputEncoding = [Text.UTF8Encoding]::new($true)
$id = if ($env:REGAIN_ACCESSORY_TEST_CLSID) { [Guid]$env:REGAIN_ACCESSORY_TEST_CLSID } elseif ($DeviceClass -eq 'EfwFilterWheel') { [Guid]'EA2040E1-E936-4BDF-87F7-B58CA3E418AB' } else { [Guid]'295C08F8-EDE9-43C5-9D55-627A063D74CA' }
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
        if ($device.SupportedActions -notcontains 'Regain.Calibrate') { throw 'Calibration action missing' }
        if ($Calibrate -or !$Hardware) {
            # Start away from the calibration endpoint so a no-op cannot pass.
            $device.Position = [int16]1
            $deadline = [DateTime]::UtcNow.AddSeconds(30)
            while ($device.Position -eq -1) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Pre-calibration move timed out' }; Start-Sleep -Milliseconds 100 }
            if ($device.Position -ne 1) { throw 'Pre-calibration position mismatch' }
            $started = [DateTime]::UtcNow
            [void]$device.Action('Regain.Calibrate', '')
            if ($CalibrationPollDelayMs) { Start-Sleep -Milliseconds $CalibrationPollDelayMs }
            $status = $device.Action('Regain.Status', '') | ConvertFrom-Json
            if ($status.fault -or $status.error -or $status.slots -ne 7) { throw 'Invalid calibration status' }
            # A busy CI runner can miss the entire two-second simulation.
            # Check one coherent snapshot, not Position from a later instant.
            # Duplicate rejection is covered by worker and Alpaca tests; issuing
            # another action here could legitimately start a second calibration.
            if ($status.calibrating) {
                if (!$status.moving -or $status.position -ne -1) { throw 'Invalid calibration progress' }
            } elseif ($status.moving -or $status.position -ne 0) { throw 'Invalid completed calibration' }
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
        if ($device.SupportedActions -contains 'Regain.Calibrate') { throw 'Focuser advertised EFW calibration' }
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
    $identity = $device.Action('Regain.Identity', '') | ConvertFrom-Json
    if (!$identity.serial) { throw 'Missing identity' }
    Write-Output ("{0}-bit COM {1}: connected, moved, restored and read identity ({2})" -f ([IntPtr]::Size * 8), $DeviceClass, $(if ($Hardware) { 'USB hardware' } else { 'simulation' }))
} finally {
    try { $device.Connected = $false } finally {
        if ([Runtime.InteropServices.Marshal]::IsComObject($device)) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device) } else { $device.Dispose() }
    }
}
