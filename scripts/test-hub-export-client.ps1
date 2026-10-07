param([Parameter(Mandatory)][string]$Directory, [Parameter(Mandatory)][string]$Role, [switch]$MetadataOnly, [switch]$TraceLaunch)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'ComTestProperty.ps1')
$ids = Get-Content -LiteralPath (Join-Path $Directory 'identities.json') -Raw | ConvertFrom-Json
$objects = @()
$wheelNames = 'L|H' + [char]0x03B1 + '|'
function Wait-Condition([scriptblock]$Condition, [string]$Step) {
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
        if ([DateTime]::UtcNow -gt $deadline) { throw "Hub export $Role condition timed out: $Step" }
        Start-Sleep -Milliseconds 25
    }
}
function Wait-Signal([string]$Name) { Wait-Condition { Test-Path -LiteralPath (Join-Path $Directory $Name) } "signal $Name" }
function Value($Device, [string]$Name) { return ,(Get-ComTestProperty -Device $Device -Name $Name) }
try {
    foreach ($identity in $ids) {
        if ($TraceLaunch -and $identity.clsid -eq $ids[0].clsid) {
            $principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
            $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey([Microsoft.Win32.RegistryHive]::ClassesRoot, [Microsoft.Win32.RegistryView]::Default)
            try {
                $key = $root.OpenSubKey("CLSID\{$($identity.clsid)}\LocalServer32")
                try { Write-Output "Private COM launch: elevated=$($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)); $($key.GetValue(''))" }
                finally { if ($key) { $key.Dispose() } }
            } finally { $root.Dispose() }
        }
        if ($TraceLaunch) { Write-Output "Private COM metadata $($identity.clsid): activate" }
        $device = [Activator]::CreateInstance([type]::GetTypeFromCLSID([guid]$identity.clsid))
        $objects += $device
        if ($TraceLaunch) { Write-Output "Private COM metadata $($identity.clsid): interface version" }
        if ((Value $device 'InterfaceVersion') -ne $identity.version) { throw 'Incorrect interface version over COM' }
        if ($TraceLaunch) { Write-Output "Private COM metadata $($identity.clsid): connection state" }
        if ((Value $device 'Connected') -or (Value $device 'Connecting')) { throw 'New COM client acquired equipment' }
        if ($TraceLaunch) { Write-Output "Private COM metadata $($identity.clsid): name" }
        if ((Value $device 'Name') -notmatch 'SIMULATION') { throw 'Missing simulation marker' }
    }
    if ($MetadataOnly) {
        if ($TraceLaunch) { Write-Output 'Private COM metadata: verify no host process through WMI' }
        $unexpected = @(Get-CimInstance Win32_Process -Filter "Name='regain-alpaca.exe'" | Where-Object { $_.CommandLine -and $_.CommandLine.Contains($Directory) })
        if ($unexpected.Count) { throw 'Metadata launched a host for the private fixture' }
        Write-Output "Hub exported metadata $([IntPtr]::Size * 8)-bit passed"; return
    }
    $primary = $objects[0]; $other = $objects[3]; $weather = $objects[2]; $safety = $objects[1]
    if ($Role -eq 'first') {
        $focuser = $objects[4]
        $focuser.Connect(); Wait-Condition { !(Value $focuser 'Connecting') } 'focuser connect completion'
        if (!(Value $focuser 'Absolute') -or (Value $focuser 'Position') -ne 50 -or (Value $focuser 'MaxIncrement') -ne 100) { throw 'COM focuser typed properties' }
        $focuser.Move(70)
        if ((Value $focuser 'Position') -ne 70) { throw 'COM focuser Move' }
        $focuser.Halt()
        $focuser.TempComp = $true
        if (!(Value $focuser 'TempComp')) { throw 'COM focuser TempComp' }
        $focuser.Disconnect(); Wait-Condition { !(Value $focuser 'Connecting') } 'focuser disconnect completion'
        $rotator = $objects[5]
        $rotator.Connect(); Wait-Condition { !(Value $rotator 'Connecting') } 'rotator connect completion'
        if (!(Value $rotator 'CanReverse') -or (Value $rotator 'MechanicalPosition') -ne 350) { throw 'COM rotator typed properties' }
        $rotator.Sync([single]42.5)
        $rotator.Reverse = $true
        if (!(Value $rotator 'Reverse') -or (Value $rotator 'Position') -ne 42.5 -or (Value $rotator 'MechanicalPosition') -ne 350) { throw 'COM rotator shared Sync/Reverse' }
        $rotator.MoveAbsolute([single]43.5)
        $rotator.MoveMechanical([single]0)
        $rotator.Move([single]-1)
        if ((Value $rotator 'Position') -ne 51.5 -or (Value $rotator 'TargetPosition') -ne 51.5) { throw 'COM rotator commands or coordinates' }
        $rotator.Halt()
        Wait-Condition { (Value (Value $rotator 'DeviceState') 'Count') -eq 3 } 'rotator cached DeviceState'
        $states = Value $rotator 'DeviceState'
        for ($index = 0; $index -lt 3; $index++) {
            $item = $states.GetType().InvokeMember('Item', [Reflection.BindingFlags]::GetProperty, $null, $states, [object[]]@($index))
            $name = Value $item 'Name'; $value = Value $item 'Value'
            if ($name -eq 'IsMoving') { if ($value -isnot [bool]) { throw 'Rotator DeviceState motion lost Boolean type' } }
            elseif ($name -in 'Position','MechanicalPosition') { if ($value -isnot [single]) { throw 'Rotator DeviceState angle lost Single type' } }
            else { throw 'Unexpected rotator DeviceState entry' }
        }
        $rotator.Disconnect(); Wait-Condition { !(Value $rotator 'Connecting') } 'rotator disconnect completion'
        $wheel = $objects[6]
        $wheel.Connect(); Wait-Condition { !(Value $wheel 'Connecting') } 'wheel connect completion'
        if (((Value $wheel 'Names') -join '|') -cne $wheelNames -or ((Value $wheel 'FocusOffsets') -join ',') -ne '-12,0,17') { throw 'COM wheel arrays lost type, Unicode, blank slot or signed offsets' }
        $wheel.Position = [int16]2
        if ((Value $wheel 'Position') -ne 2) { throw 'COM wheel position write' }
        Wait-Condition {
            $states = Value $wheel 'DeviceState'
            if ((Value $states 'Count') -ne 1) { return $false }
            $item = $states.GetType().InvokeMember('Item', [Reflection.BindingFlags]::GetProperty, $null, $states, [object[]]@(0))
            $value = Value $item 'Value'
            return (Value $item 'Name') -eq 'Position' -and $value -is [int16] -and $value -eq 2
        } 'wheel cached short DeviceState'
        $wheel.Disconnect(); Wait-Condition { !(Value $wheel 'Connecting') } 'wheel disconnect completion'
        $panel = $objects[7]
        $panel.Connect(); Wait-Condition { !(Value $panel 'Connecting') } 'panel connect completion'
        if ((Value $panel 'MaxBrightness') -ne 4096 -or (Value $panel 'CoverState') -ne 1) { throw 'COM panel typed initial properties' }
        $panel.OpenCover()
        $panel.CalibratorOn(0)
        if ((Value $panel 'Brightness') -ne 0 -or (Value $panel 'CalibratorState') -ne 3 -or (Value $panel 'CalibratorChanging') -or (Value $panel 'CoverMoving')) { throw 'COM panel logical zero-on or completion' }
        $panel.CalibratorOn(17)
        Wait-Condition {
            $states = Value $panel 'DeviceState'
            if ((Value $states 'Count') -ne 5) { return $false }
            $brightnessReady = $false
            for ($index = 0; $index -lt 5; $index++) {
                $item = $states.GetType().InvokeMember('Item', [Reflection.BindingFlags]::GetProperty, $null, $states, [object[]]@($index))
                $name = Value $item 'Name'; $value = Value $item 'Value'
                if ($name -in 'CoverMoving','CalibratorChanging') { if ($value -isnot [bool]) { throw 'Panel DeviceState completion lost Boolean type' } }
                elseif ($name -eq 'Brightness') {
                    if ($value -isnot [int]) { throw 'Panel DeviceState brightness lost Int32 type' }
                    $brightnessReady = $value -eq 17
                } elseif ($name -in 'CoverState','CalibratorState') {
                    $expected = if ($name -eq 'CoverState') { 'ASCOM.DeviceInterface.CoverStatus' } else { 'ASCOM.DeviceInterface.CalibratorStatus' }
                    # IStateValue.Value is Object/VARIANT. A boxed Int32 enum
                    # crosses Automation as VT_I4; a managed CCW shortcut may
                    # retain the enum. Direct managed clients check its declared
                    # enum type separately. Never accept strings/Short/Double.
                    $typedEnum = $value.GetType().FullName -eq $expected -and [Enum]::GetUnderlyingType($value.GetType()) -eq [int]
                    if ((!$typedEnum -and $value -isnot [int]) -or [int]$value -lt 0 -or [int]$value -gt 5) { throw "Panel DeviceState $name expected $expected or VT_I4 with bounds 0..5, received $($value.GetType().AssemblyQualifiedName) value=$value" }
                } else { throw 'Unexpected panel DeviceState entry' }
            }
            return $brightnessReady
        } 'panel cached typed DeviceState'
        $panel.Disconnect(); Wait-Condition { !(Value $panel 'Connecting') } 'panel disconnect completion'
    }
    if ($Role -eq 'second') {
        Wait-Signal 'first-connected'
        $rotator = $objects[5]; $rotator.Connected = $true
        $rotator.MoveAbsolute([single]51.5)
        if ((Value $rotator 'Position') -ne 51.5 -or !(Value $rotator 'Reverse')) { throw 'Second COM bitness lost rotator state' }
        $rotator.Connected = $false
        $wheel = $objects[6]; $wheel.Connected = $true
        if ((Value $wheel 'Position') -ne 2 -or ((Value $wheel 'Names') -join '|') -cne $wheelNames) { throw 'Second COM bitness lost wheel state or arrays' }
        $wheel.Connected = $false
        $panel = $objects[7]; $panel.Connected = $true
        if ((Value $panel 'Brightness') -ne 17 -or (Value $panel 'CalibratorState') -ne 3 -or (Value $panel 'CoverState') -ne 3) { throw 'Second COM bitness lost shared panel state' }
        $panel.CloseCover(); $panel.CalibratorOff()
        if ((Value $panel 'Brightness') -ne 0 -or (Value $panel 'CalibratorState') -ne 1 -or (Value $panel 'CoverState') -ne 1) { throw 'COM panel explicit Close/Off' }
        $panel.Connected = $false
    }
    Write-Output "Hub export ${Role}: connect primary"
    $primary.Connect(); Wait-Condition { !(Value $primary 'Connecting') } 'primary connect completion'
    if (!(Value $primary 'Connected')) { throw 'Modern connection did not acquire output' }
    $other.Connected = $true
    if ((Value $primary 'MaxSwitch') -ne 3) { throw 'Switch dispatch failed' }
    if ($Role -eq 'first') {
        $primary.SetSwitchValue(1, 46)
        Wait-Condition { $other.GetSwitchValue(1) -eq 46 } 'shared value 46 on sibling output'
        'ready' | Set-Content -LiteralPath (Join-Path $Directory 'first-connected')
        Wait-Signal 'second-connected'
        $primary.Disconnect(); Wait-Condition { !(Value $primary 'Connecting') } 'primary disconnect completion'
        $other.Connected = $false
        'done' | Set-Content -LiteralPath (Join-Path $Directory 'first-disconnected')
        Wait-Signal 'second-finished'
    } else {
        Wait-Condition { $primary.GetSwitchValue(1) -eq 46 } 'shared value 46 on primary output'
        'ready' | Set-Content -LiteralPath (Join-Path $Directory 'second-connected')
        Wait-Signal 'first-disconnected'
        if (!(Value $primary 'Connected') -or $other.GetSwitchValue(1) -ne 46) { throw 'Client disconnect revoked sibling output' }
        $other.SetSwitchValue(1, 72); Wait-Condition { $primary.GetSwitchValue(1) -eq 72 } 'shared value 72'
        $weather.Connect(); Wait-Condition { !(Value $weather 'Connecting') } 'weather connect completion'
        Wait-Condition { $weather.GetType().InvokeMember('Temperature', [Reflection.BindingFlags]::GetProperty, $null, $weather, $null) -eq 12 } 'weather temperature 12'
        $safety.Connected = $true
        if ((Value $safety 'IsSafe')) { throw 'Simulation safety did not start unsafe' }
        $states = Value $safety 'DeviceState'
        if ((Value $states 'Count') -ne 1) { throw 'DeviceState collection did not cross COM boundary' }
        $state = $states.GetType().InvokeMember('Item', [Reflection.BindingFlags]::GetProperty, $null, $states, [object[]]@(0))
        $safeState = Value $state 'Value'
        if ((Value $state 'Name') -ne 'IsSafe' -or $safeState -isnot [bool] -or $safeState) {
            throw 'DeviceState item lost its name or boolean value across COM'
        }
        $weather.Disconnect(); Wait-Condition { !(Value $weather 'Connecting') } 'weather disconnect completion'
        $safety.Connected = $false; $primary.Connected = $false; $other.Connected = $false
        'done' | Set-Content -LiteralPath (Join-Path $Directory 'second-finished')
    }
    Write-Output "Hub exported outputs $Role $([IntPtr]::Size * 8)-bit passed"
} finally {
    if ($TraceLaunch) { Write-Output 'Private COM metadata: release objects' }
    foreach ($device in $objects) { try { $device.Dispose() } catch { }; [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($device) }
    if ($TraceLaunch) { Write-Output 'Private COM metadata: cleanup complete' }
}
