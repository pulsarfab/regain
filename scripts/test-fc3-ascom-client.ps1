param([string]$Id, [string]$Directory, [string]$Role, [switch]$MetadataOnly)
$ErrorActionPreference = 'Stop'
function Wait-Signal([string]$Name) {
    $deadline = [DateTime]::UtcNow.AddSeconds(25)
    while (!(Test-Path -LiteralPath (Join-Path $Directory $Name))) {
        if ([DateTime]::UtcNow -gt $deadline) { throw "Timed out waiting for $Name" }
        Start-Sleep -Milliseconds 100
    }
}
$device = $null
try {
    $type = if ($Id) { [type]::GetTypeFromCLSID([Guid]$Id) } else { [type]::GetTypeFromProgID("ASCOM.ZWOgain.FocusCube3.Focuser") }
    $device = [Activator]::CreateInstance($type)
    if ($MetadataOnly) { if ($device.Name -ne "PulsarFab regain Pegasus FocusCube3" -or $device.InterfaceVersion -ne 3) { throw "Invalid metadata" }; $device.Dispose(); return }
    if ($device.Connected) { throw 'A new COM client inherited another connection' }
    $device.Connected = $true
    if (!$device.Absolute -or $device.MaxStep -ne 1000000) { throw 'Bad IFocuser properties' }
    $identity = $device.Action('Regain.Identity','') | ConvertFrom-Json
    if ($identity.model -ne 'Pegasus Astro FocusCube3') { throw 'Wrong identity' }
    $device.Action('Regain.Status','') | Set-Content (Join-Path $Directory "$Role-connected")
    if ($Role -eq 'first') {
        Wait-Signal 'second-connected'
        $servers = @(Get-CimInstance Win32_Process -Filter "Name='Regain.Pegasus.ASCOM.exe'" | Where-Object { $_.CommandLine -like "*$Id*" })
        if ($servers.Count -ne 1) { throw 'Expected one shared COM server' }
        $workers = @(Get-CimInstance Win32_Process -Filter "Name='regain-device.exe'" | Where-Object ParentProcessId -eq $servers[0].ProcessId)
        if ($workers.Count -ne 1) { throw 'Two ASCOM clients must share exactly one worker' }
        if ($env:REGAIN_ACCESSORY_SIMULATE -eq '1') {
            if (!$device.TempCompAvailable -or $device.TempComp) { throw 'Invalid initial compensation state' }
            $device.TempComp = $true
        }
        $device.Connected = $false
        if ($device.Connected) { throw 'Disconnect failed' }
        'done' | Set-Content (Join-Path $Directory 'first-disconnected')
        Wait-Signal 'second-finished'
    } else {
        Wait-Signal 'first-disconnected'
        if (!$device.Connected) { throw 'First client disconnected second client' }
        if ($env:REGAIN_ACCESSORY_SIMULATE -eq '1' -and !$device.TempComp) { throw 'Shared compensation state was lost when another client disconnected' }
        if ($env:REGAIN_ACCESSORY_SIMULATE -eq '1') {
            $rejected = $false
            try { $device.Move(0) } catch { $rejected = $true; if ($_.Exception.GetBaseException().HResult -ne -2147220479) { throw } }
            if (!$rejected -or $device.IsMoving -or !$device.TempComp) { throw 'Backlash range preflight violated the ASCOM contract' }
        }
        $initial = $device.Position
        $device.Move($initial + 20)
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        while ($device.IsMoving) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Move timeout' }; Start-Sleep -Milliseconds 100 }
        if ($device.Position -ne ($initial + 20)) { throw 'Wrong final position' }
        if ($env:REGAIN_ACCESSORY_SIMULATE -eq '1') {
            $state = $device.Action('Regain.Status','') | ConvertFrom-Json
            if (!$device.TempComp -or $state.temperature_compensation.referencePosition -ne ($initial + 20)) { throw 'Enabled Move did not rebase compensation' }
        }
        $device.Move($initial)
        while ($device.IsMoving) { if ([DateTime]::UtcNow -gt $deadline) { throw 'Return timeout' }; Start-Sleep -Milliseconds 100 }
        $device.Halt()
        if ($env:REGAIN_ACCESSORY_SIMULATE -eq '1' -and $device.TempComp) { throw 'Halt did not stop compensation' }
        $device.Connected = $false
        'done' | Set-Content (Join-Path $Directory 'second-finished')
    }
    $device.Dispose()
    Write-Output "FocusCube3 COM $Role ($([IntPtr]::Size * 8)-bit) passed"
} catch { Write-Error $_; exit 1 }
finally { if ($null -ne $device) { try { $device.Connected=$false } catch { }; [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device) } }
