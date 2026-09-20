param(
    [Parameter(Mandatory)][string]$Port,
    [switch]$Exercise,
    [switch]$MotionDetails,
    [string]$OutputPath = 'artifacts/inspection/ofp2-serial.jsonl'
)
$ErrorActionPreference = 'Stop'
# Commands and serial settings recovered from installed DeepSkyDad FP 1.0.3.6.
# Records application-level serial bytes, not USB bus packets. No vendor code needed.
New-Item -ItemType Directory -Force (Split-Path $OutputPath) | Out-Null
$serial = [System.IO.Ports.SerialPort]::new($Port,115200,[System.IO.Ports.Parity]::None,8,[System.IO.Ports.StopBits]::One)
$serial.Handshake = [System.IO.Ports.Handshake]::RequestToSend
$serial.DtrEnable = $true
$serial.ReadTimeout = 2000
$serial.WriteTimeout = 2000
function Send([string]$Command) {
    Start-Sleep -Milliseconds 50
    $wire = '[' + $Command + ']'
    $serial.Write($wire)
    try { $reply = $serial.ReadTo(')') + ')' }
    catch {
        @{time=[DateTime]::UtcNow.ToString('o');tx=$wire;rx=$null;error=$_.Exception.Message} |
            ConvertTo-Json -Compress | Add-Content -LiteralPath $OutputPath -Encoding utf8
        throw
    }
    $record = @{time=[DateTime]::UtcNow.ToString('o');tx=$wire;rx=$reply}
    $line = $record | ConvertTo-Json -Compress
    Add-Content -LiteralPath $OutputPath -Value $line -Encoding utf8
    Write-Host $line
    if ($reply.StartsWith('!')) { throw "Device rejected $Command : $reply" }
    return $reply.TrimStart('(').TrimEnd(')')
}
function Wait-Cover([int]$Expected) {
    $deadline = [DateTime]::UtcNow.AddSeconds(120)
    do {
        $state = [int](Send GOPS)
        $position = [int](Send GPOS)
        $endpoint = if ($Expected -eq 0) { 270 } else { 0 }
        if ($state -eq $Expected -and $position -eq $endpoint) { return }
        if ($state -ne 2) { throw "Unexpected cover state $state" }
        Start-Sleep -Milliseconds 500
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Cover motion timed out'
}
try {
    $serial.Open()
    Start-Sleep -Milliseconds 1500
    $serial.DiscardInBuffer()
    $firmware = Send GFRM
    if (-not $firmware.StartsWith('Board=DeepSkyDad.FP2')) { throw 'Not an FP2 board' }
    if ((Send GPRD) -ne '3') { throw 'Not an OFP2 product' }
    $cover = [int](Send GOPS)
    $position = [int](Send GPOS)
    $brightness = [int](Send GLBR)
    $light = [int](Send GLON)
    if ($MotionDetails) { Send GMOV | Out-Null; Send GPOS | Out-Null }
    if ($Exercise) {
        if (-not (($cover -eq 0 -and $position -eq 270) -or ($cover -eq 1 -and $position -eq 0))) { throw 'Start with a stationary cover at an endpoint' }
        try {
            Send SLBR128 | Out-Null
            Send SLON1 | Out-Null
            Send GLBR | Out-Null
            Send GLON | Out-Null
            Send SLBR0 | Out-Null
            Send SLON0 | Out-Null
            Send GLON | Out-Null
            $target = if ($cover -eq 0) { 0 } else { 270 }
            Send "STRG$target" | Out-Null
            Send SMOV | Out-Null
            Start-Sleep -Milliseconds 750
            Send GOPS | Out-Null
            Send STOP | Out-Null
            Start-Sleep -Milliseconds 1500
            Send GOPS | Out-Null
            if ($MotionDetails) { Send GMOV | Out-Null; Send GPOS | Out-Null }
            Send "STRG$target" | Out-Null
            Send SMOV | Out-Null
            Wait-Cover (1-$cover)
        } finally {
            Send STOP | Out-Null
            $restore = if ($cover -eq 0) { 270 } else { 0 }
            Send "STRG$restore" | Out-Null
            Send SMOV | Out-Null
            Wait-Cover $cover
            Send "SLBR$brightness" | Out-Null
            Send "SLON$light" | Out-Null
        }
    }
} finally { $serial.Dispose() }
