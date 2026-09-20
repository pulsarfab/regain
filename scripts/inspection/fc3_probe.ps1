# Application-level serial tracing. No vendor code or Wi-Fi credentials are collected.
param([Parameter(Mandatory)][string]$Port, [string]$Output = 'artifacts/fc3-serial.jsonl', [switch]$Move)
$ErrorActionPreference='Stop'
$serial=[IO.Ports.SerialPort]::new($Port,115200,[IO.Ports.Parity]::None,8,[IO.Ports.StopBits]::One)
$serial.DtrEnable=$true; $serial.Handshake=[IO.Ports.Handshake]::None
$serial.ReadTimeout=2000; $serial.WriteTimeout=2000; $serial.NewLine="`n"
$records=[Collections.Generic.List[object]]::new()
function Exchange([string]$Command) {
    $serial.WriteLine($Command)
    $reply=$serial.ReadLine().TrimEnd("`r")
    $records.Add(@{timestamp=[DateTimeOffset]::UtcNow.ToString('O'); command=$Command; response=$reply})
    $reply
}
function Wait-Idle([int]$Target) {
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do {
        $state=(Exchange 'FA').Split(':')
        if ($state[2] -eq '0') { if ([int]$state[1] -ne $Target) { throw 'Stopped before target' }; return }
        if([DateTime]::UtcNow -gt $deadline) { throw 'Movement timeout' }
        Start-Sleep -Milliseconds 100
    } while($true)
}
try {
    $serial.Open(); Start-Sleep -Milliseconds 1500; $serial.DiscardInBuffer()
    $identity=Exchange 'F#'
    if (!$identity.StartsWith('FC3')) { throw "Unexpected identity $identity" }
    foreach($c in 'FV','FA','SP','FP','FT','FI','FU') { Write-Output "$c => $(Exchange $c)" }
    if($Move) {
        $initial=(Exchange 'FA').Split(':')
        if($initial[2] -ne '0') { throw 'Already moving' }
        $position=[int]$initial[1]
        $speed=Exchange 'SP'
        try {
            try {
                foreach($c in 'SP:399','BL:5','FD:1') { [void](Exchange $c) }
                [void](Exchange 'FA')
            } finally {
                foreach($c in @("SP:$speed","BL:$($initial[5])","FD:$($initial[4])")) { [void](Exchange $c) }
            }
            [void](Exchange "FM:$($position+50)"); Wait-Idle ($position+50)
            [void](Exchange "FM:$position"); Wait-Idle $position
            [void](Exchange 'FH')
        } finally { [void](Exchange 'FH'); [void](Exchange "FM:$position"); Wait-Idle $position }
    }
} finally {
    $serial.Dispose()
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Output))) | Out-Null
    $records | ForEach-Object { $_ | ConvertTo-Json -Compress } | Set-Content -LiteralPath $Output -Encoding utf8
}
