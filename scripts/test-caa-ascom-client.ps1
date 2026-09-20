param([switch]$Hardware)
$ErrorActionPreference = 'Stop'
$id = [Guid]'A918164B-49DD-4FF5-BEE6-A4AB93B97F12'
if ($env:REGAIN_CAA_TEST_CLSID) { $id = [Guid]$env:REGAIN_CAA_TEST_CLSID }
$driver = [Activator]::CreateInstance([Type]::GetTypeFromCLSID($id))
function Wait-Rotator {
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ($driver.IsMoving) { if ([DateTime]::UtcNow -gt $deadline) { $driver.Halt(); throw 'CAA move timed out' }; Start-Sleep -Milliseconds 100 }
}
$initial = $null
$originChanged = $false
try {
    if ($driver.Name -ne 'PulsarFab regain CAA Rotator' -or $driver.InterfaceVersion -ne 3) { throw 'Wrong CAA metadata' }
    if ($driver.SupportedActions -notcontains 'Regain.CAA.ResetOrigin') { throw 'Missing origin reset action' }
    if (!$Hardware) { Write-Output ('{0}-bit CAA COM metadata passed' -f ([IntPtr]::Size * 8)); return }
    $driver.Connected = $true
    $initial = $driver.Action('Regain.CAA.Status','') | ConvertFrom-Json
    if ($initial.moving -or $initial.error) { throw 'CAA is not idle' }
    $driver.Sync(40)
    $originChanged = $true
    $driver.Action('Regain.CAA.ResetOrigin','') | Out-Null
    if ([Math]::Abs($driver.MechanicalPosition) -gt .03 -or [Math]::Abs($driver.Position - 40) -gt .03) { throw 'Origin reset or logical preservation failed' }
    $driver.Action('Regain.CAA.SetReference', ('{"degrees":' + $initial.mechanical_degrees.ToString([Globalization.CultureInfo]::InvariantCulture) + '}')) | Out-Null
    $originChanged = $false
    $driver.MoveMechanical([float]($initial.mechanical_degrees + 1)); Wait-Rotator
    if ([Math]::Abs($driver.MechanicalPosition - $initial.mechanical_degrees - 1) -gt .1) { throw 'Mechanical move failed' }
    $driver.MoveMechanical([float]$initial.mechanical_degrees); Wait-Rotator
    $driver.Action('Regain.CAA.SetLimit','{"degrees":361}') | Out-Null
    if (($driver.Action('Regain.CAA.Status','') | ConvertFrom-Json).limit_degrees -ne 361) { throw 'Extended limit failed' }
    $driver.Action('Regain.CAA.SetLimit','{"degrees":360}') | Out-Null
    Write-Output ('{0}-bit CAA COM: connect, sync, origin reset, reference restore, motion, extended limit passed' -f ([IntPtr]::Size * 8))
} finally {
    try {
        if ($Hardware -and $driver.Connected) {
            $driver.Halt()
            if ($null -ne $initial) {
                if ($originChanged) { $driver.Action('Regain.CAA.SetReference', ('{"degrees":' + $initial.mechanical_degrees.ToString([Globalization.CultureInfo]::InvariantCulture) + '}')) | Out-Null }
                $driver.Action('Regain.CAA.SetLimit', ('{"degrees":' + $initial.limit_degrees + '}')) | Out-Null
                $driver.MoveMechanical([float]$initial.mechanical_degrees); Wait-Rotator
                $driver.Sync([float]$initial.logical_degrees)
            }
            $driver.Connected = $false
        }
    } finally {
        if ([Runtime.InteropServices.Marshal]::IsComObject($driver)) { [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($driver) } else { $driver.Dispose() }
    }
}
