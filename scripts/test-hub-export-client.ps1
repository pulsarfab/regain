param([Parameter(Mandatory)][string]$Directory, [Parameter(Mandatory)][string]$Role, [switch]$MetadataOnly)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'ComTestProperty.ps1')
$ids = Get-Content -LiteralPath (Join-Path $Directory 'identities.json') -Raw | ConvertFrom-Json
$objects = @()
function Wait-Condition([scriptblock]$Condition) {
    $deadline = [DateTime]::UtcNow.AddSeconds(15)
    while ($true) {
        try { if (& $Condition) { return } }
        catch {
            $cause = $_.Exception
            while ($cause.InnerException) { $cause = $cause.InnerException }
            # ValueNotSet means no real sample yet. Completion failures and
            # command errors never count as successful data.
            if ($cause.HResult -ne -2147220478) { throw } # ASCOM 0x80040402
        }
        if ([DateTime]::UtcNow -gt $deadline) { throw 'Hub export condition timed out' }
        Start-Sleep -Milliseconds 25
    }
}
function Wait-Signal([string]$Name) { Wait-Condition { Test-Path -LiteralPath (Join-Path $Directory $Name) } }
function Value($Device, [string]$Name) { Get-ComTestProperty -Device $Device -Name $Name }
try {
    foreach ($identity in $ids) {
        $device = [Activator]::CreateInstance([type]::GetTypeFromCLSID([guid]$identity.clsid))
        $objects += $device
        if ((Value $device 'InterfaceVersion') -ne $identity.version) { throw 'Incorrect interface version over COM' }
        if ((Value $device 'Connected') -or (Value $device 'Connecting')) { throw 'New COM client acquired equipment' }
        if ((Value $device 'Name') -notmatch 'SIMULATION') { throw 'Missing simulation marker' }
    }
    if ($MetadataOnly) {
        $unexpected = @(Get-CimInstance Win32_Process -Filter "Name='regain-alpaca.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($Directory) })
        if ($unexpected.Count) { throw 'Metadata launched a host for the private fixture' }
        Write-Output "Hub exported metadata $([IntPtr]::Size * 8)-bit passed"; return
    }
    $primary = $objects[0]; $other = $objects[3]; $weather = $objects[2]; $safety = $objects[1]
    if ($Role -eq 'second') { Wait-Signal 'first-connected' }
    $primary.Connect(); Wait-Condition { !(Value $primary 'Connecting') }
    if (!(Value $primary 'Connected')) { throw 'Modern connection did not acquire output' }
    $other.Connected = $true
    if ((Value $primary 'MaxSwitch') -ne 3) { throw 'Switch dispatch failed' }
    if ($Role -eq 'first') {
        $primary.SetSwitchValue(1, 46)
        Wait-Condition { $other.GetSwitchValue(1) -eq 46 }
        'ready' | Set-Content -LiteralPath (Join-Path $Directory 'first-connected')
        Wait-Signal 'second-connected'
        $primary.Disconnect(); Wait-Condition { !(Value $primary 'Connecting') }
        $other.Connected = $false
        'done' | Set-Content -LiteralPath (Join-Path $Directory 'first-disconnected')
        Wait-Signal 'second-finished'
    } else {
        Wait-Condition { $primary.GetSwitchValue(1) -eq 46 }
        'ready' | Set-Content -LiteralPath (Join-Path $Directory 'second-connected')
        Wait-Signal 'first-disconnected'
        if (!(Value $primary 'Connected') -or $other.GetSwitchValue(1) -ne 46) { throw 'Client disconnect revoked sibling output' }
        $other.SetSwitchValue(1, 72); Wait-Condition { $primary.GetSwitchValue(1) -eq 72 }
        $weather.Connect(); Wait-Condition { !(Value $weather 'Connecting') }
        Wait-Condition { $weather.GetType().InvokeMember('Temperature', [Reflection.BindingFlags]::GetProperty, $null, $weather, $null) -eq 12 }
        $safety.Connected = $true
        if ((Value $safety 'IsSafe')) { throw 'Simulation safety did not start unsafe' }
        if ((Value $safety 'DeviceState').Count -ne 1) { throw 'DeviceState collection did not cross COM boundary' }
        $weather.Disconnect(); Wait-Condition { !(Value $weather 'Connecting') }
        $safety.Connected = $false; $primary.Connected = $false; $other.Connected = $false
        'done' | Set-Content -LiteralPath (Join-Path $Directory 'second-finished')
    }
    Write-Output "Hub exported outputs $Role $([IntPtr]::Size * 8)-bit passed"
} finally {
    foreach ($device in $objects) { try { $device.Dispose() } catch { }; [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device) }
}
