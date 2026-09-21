param([string]$Id, [string]$Directory, [string]$Role, [switch]$MetadataOnly)
$ErrorActionPreference = 'Stop'
function Wait-Signal([string]$Name) {
    $deadline = [DateTime]::UtcNow.AddSeconds(150)
    while (!(Test-Path -LiteralPath (Join-Path $Directory $Name))) {
        if ([DateTime]::UtcNow -gt $deadline) { throw "Timed out waiting for $Name" }
        Start-Sleep -Milliseconds 100
    }
}
function Assert-Rejected([scriptblock]$Action) {
    $rejected = $false
    try { & $Action | Out-Null } catch { $rejected = $true }
    if (!$rejected) { throw "Expected the driver to reject: $Action" }
}
function Wait-Cover([int]$Expected) {
    $deadline = [DateTime]::UtcNow.AddSeconds(125)
    while ([int]$device.CoverState -eq 2) {
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Cover movement timed out' }
        Start-Sleep -Milliseconds 200
    }
    if ([int]$device.CoverState -ne $Expected) { throw "Expected cover state $Expected, got $($device.CoverState)" }
}
$device = $null
$initial = $null
try {
    $type = if ($Id) { [type]::GetTypeFromCLSID([Guid]$Id) } else { [type]::GetTypeFromProgID('ASCOM.Regain.OFP2.CoverCalibrator') }
    $device = [Activator]::CreateInstance($type)
    if ($device.Name -ne 'PulsarFab regain Deep Sky Dad OFP2' -or $device.InterfaceVersion -ne 1) { throw 'Invalid CoverCalibrator metadata' }
    if ($MetadataOnly) { $device.Dispose(); return }
    if ($device.Connected) { throw 'New COM client inherited another connection' }
    Assert-Rejected { $device.GetType().InvokeMember('Brightness', [Reflection.BindingFlags]::GetProperty, $null, $device, $null) }
    Assert-Rejected { $device.OpenCover() }
    Assert-Rejected { $device.CalibratorOn(1) }
    if ($Role -eq 'second') { Wait-Signal 'first-connected' }
    $device.Connected = $true
    $device.Connected = $true # Repeated connects must not leak leases.
    if ($device.MaxBrightness -ne 4096) { throw 'Wrong maximum brightness' }
    if (($device.Action('Regain.Identity','') | ConvertFrom-Json).model -ne 'Deep Sky Dad OFP2') { throw 'Wrong identity' }
    Assert-Rejected { $device.CalibratorOn(-1) }
    Assert-Rejected { $device.CalibratorOn(4097) }
    Assert-Rejected { $device.Action('Unsupported','') }
    if ($Role -eq 'first') {
        $initial = $device.Action('Regain.Status','') | ConvertFrom-Json
        $initial | ConvertTo-Json | Set-Content (Join-Path $Directory 'initial.json')
        $device.CalibratorOn(128)
        'ready' | Set-Content (Join-Path $Directory 'first-connected')
        Wait-Signal 'second-connected'
        $servers = @(Get-CimInstance Win32_Process -Filter "Name='Regain.Ofp2.ASCOM.exe'" | Where-Object { $_.CommandLine -like "*$Id*" })
        if ($servers.Count -ne 1) { throw 'Expected one shared COM server' }
        $workers = @(Get-CimInstance Win32_Process -Filter "Name='regain-device.exe'" | Where-Object ParentProcessId -eq $servers[0].ProcessId)
        if ($workers.Count -ne 1) { throw 'Two ASCOM clients must share exactly one serial worker' }
        $device.Connected = $false
        if ($device.Connected) { throw 'Disconnect failed' }
        Assert-Rejected { $device.GetType().InvokeMember('Brightness', [Reflection.BindingFlags]::GetProperty, $null, $device, $null) }
        $device.Dispose()
        'done' | Set-Content (Join-Path $Directory 'first-disconnected')
        Wait-Signal 'second-finished'
    } else {
        $initial = Get-Content (Join-Path $Directory 'initial.json') -Raw | ConvertFrom-Json
        if ($device.Brightness -ne 128 -or [int]$device.CalibratorState -ne 3) { throw 'Clients do not share light state' }
        'ready' | Set-Content (Join-Path $Directory 'second-connected')
        Wait-Signal 'first-disconnected'
        if (!$device.Connected -or $device.Brightness -ne 128) { throw 'First client disconnected second client' }
        $device.CalibratorOn(0)
        if ($device.Brightness -ne 0 -or [int]$device.CalibratorState -ne 3) { throw 'CalibratorOn(0) must be Ready at zero brightness' }
        $device.CalibratorOff()
        if ($device.Brightness -ne 0 -or [int]$device.CalibratorState -ne 1) { throw 'CalibratorOff must be Off at zero brightness' }
        $device.CloseCover(); Wait-Cover 1
        $device.OpenCover(); Wait-Cover 3
        $device.HaltCover()
        if ($initial.cover -eq 'closed') { $device.CloseCover(); Wait-Cover 1 }
        if ($initial.calibrator_on) { $device.CalibratorOn([int]$initial.brightness) } else { $device.CalibratorOff() }
        $device.Dispose() # Dispose, rather than Connected=false, releases the last lease.
        if ($device.Connected) { throw 'Disposed client is connected' }
        Assert-Rejected { $device.Connected = $true }
        'done' | Set-Content (Join-Path $Directory 'second-finished')
    }
    Write-Output "OFP2 COM $Role ($([IntPtr]::Size * 8)-bit) passed"
} catch { Write-Error $_; exit 1 }
finally {
    if ($null -ne $device) {
        try { if ($Role -eq 'second' -and $device.Connected) { $device.HaltCover(); if ($initial.calibrator_on) { $device.CalibratorOn([int]$initial.brightness) } else { $device.CalibratorOff() } }; $device.Connected = $false } catch { }
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device)
    }
}
