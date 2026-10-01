param([string]$Port='COM7', [string]$Output='artifacts/falcon-serial.jsonl', [switch]$Move)
$ErrorActionPreference='Stop'
$serial=[IO.Ports.SerialPort]::new($Port,115200,[IO.Ports.Parity]::None,8,[IO.Ports.StopBits]::One)
$serial.DtrEnable=$true; $serial.RtsEnable=$true; $serial.Handshake=[IO.Ports.Handshake]::None
$serial.ReadTimeout=2000; $serial.WriteTimeout=2000; $serial.NewLine="`n"
$records=[Collections.Generic.List[object]]::new()
$motionMayBeActive=$false
function Exchange([string]$Command) {
    if ($Command.StartsWith('MD:')) { $script:motionMayBeActive=$true }
    $serial.WriteLine($Command)
    $reply=$serial.ReadLine().TrimEnd("`r")
    $records.Add(@{timestamp=[DateTimeOffset]::UtcNow.ToString('O'); command=$Command; response=$reply})
    Start-Sleep -Milliseconds 50
    $reply
}
function Wait-Idle {
    $deadline=[DateTime]::UtcNow.AddSeconds(120)
    do {
        $state=(Exchange 'FA').Split(':')
        if($state[2] -eq '0') { $script:motionMayBeActive=$false; return $state[1] }
        if([DateTime]::UtcNow -gt $deadline) { throw 'Movement timeout' }
        Start-Sleep -Milliseconds 200
    } while($true)
}
try {
    $serial.Open(); Start-Sleep -Milliseconds 1500; $serial.DiscardInBuffer()
    $identity=Exchange 'F#'
    if (!$identity.StartsWith('F2R_')) { throw 'Not a Falcon V2' }
    foreach($c in 'FV','FA','FD','FR','FS','FU') { Write-Output "$c => $(Exchange $c)" }
    if($Move) {
        if((Exchange 'FR') -ne 'FR:0') { throw 'Already moving' }
        foreach($c in 'MD:5','MD:0','SD:359','MD:1','SD:0','MD:5','MD:0') {
            Write-Output "$c => $(Exchange $c)"
            Write-Output "Idle at $(Wait-Idle)"
        }
        Write-Output "FH => $(Exchange 'FH')"
        Write-Output "SD:0 => $(Exchange 'SD:0')"
    }
} finally {
    if ($motionMayBeActive -and $serial.IsOpen) {
        # A failed exchange can leave a stale reply: request a stop once without
        # interpreting its acknowledgement as proof that the motor stopped.
        try { $serial.WriteLine('FH') } catch { Write-Warning "Best-effort halt failed: $_" }
    }
    $serial.Dispose()
    $records | ForEach-Object { $_ | ConvertTo-Json -Compress } | Set-Content -LiteralPath $Output -Encoding utf8
}
