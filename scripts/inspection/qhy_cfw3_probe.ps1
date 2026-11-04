param(
    [Parameter(Mandatory)][string]$Port,
    [Parameter(Mandatory)][string]$OutputDirectory,
    [ValidateRange(2,16)][int]$ExpectedSlots,
    [switch]$Sweep
)
# QHY's documented CFW3 bare-ASCII USB serial protocol. Never auto-scan ports,
# replay an uncertain move, change firmware, or reset/calibrate the carousel.
$ErrorActionPreference = 'Stop'
if($Sweep -and !$ExpectedSlots) {throw 'Supply the known physical carousel slot count for a sweep'}
if (Test-Path -LiteralPath $OutputDirectory) { throw 'Use a new evidence directory' }
New-Item -ItemType Directory -Path $OutputDirectory | Out-Null
$trace = Join-Path $OutputDirectory 'serial.jsonl'
$serial = [System.IO.Ports.SerialPort]::new($Port,9600,[System.IO.Ports.Parity]::None,8,[System.IO.Ports.StopBits]::One)
$serial.Handshake = [System.IO.Ports.Handshake]::None
$serial.DtrEnable = $false
$serial.RtsEnable = $false
$serial.ReadTimeout = 250
$serial.WriteTimeout = 2000
function Record($entry) {
    $entry.utc = [DateTime]::UtcNow.ToString('o')
    $entry | ConvertTo-Json -Compress -Depth 5 | Add-Content -LiteralPath $trace -Encoding utf8
}
function Exchange([string]$Command,[int]$Length,[int]$TimeoutSeconds=5) {
    if ($serial.BytesToRead -ne 0) { throw 'Unexpected pending serial response; refusing to send another command' }
    $clock = [Diagnostics.Stopwatch]::StartNew()
    Record @{direction='out';ascii=$Command;hex=[Convert]::ToHexString([Text.Encoding]::ASCII.GetBytes($Command))}
    $serial.Write($Command)
    $reply = [Collections.Generic.List[byte]]::new()
    while($reply.Count -lt $Length -and $clock.Elapsed.TotalSeconds -lt $TimeoutSeconds) {
        try { $reply.Add([byte]$serial.ReadByte()) } catch [TimeoutException] { }
    }
    $bytes=$reply.ToArray()
    Record @{direction='in';ascii=[Text.Encoding]::ASCII.GetString($bytes);hex=[Convert]::ToHexString($bytes);elapsedMs=$clock.Elapsed.TotalMilliseconds}
    if($bytes.Length -ne $Length) { throw "Incomplete reply to '$Command': $($bytes.Length)/$Length; command not retried" }
    return [Text.Encoding]::ASCII.GetString($bytes)
}
function Position([string]$Text) {
    if($Text -cnotmatch '^[0-9A-F]$') {throw "Invalid position byte '$Text'"}
    return [Convert]::ToInt32($Text,16)
}
try {
    $serial.Open()
    $version=Exchange 'VRS' 8
    if($version -notmatch '^20[0-9]{6}$') { throw "Not a recognized QHY CFW3 version: '$version'" }
    $maximumReply=Exchange 'MXP' 1
    $maximum=Position $maximumReply
    $initial=Position (Exchange 'NOW' 1)
    # Published protocol describes max index; deployed drivers also interpret
    # this response as a count. Record the raw byte and require physical geometry
    # for motion, rather than issuing a potentially out-of-range test move.
    if($ExpectedSlots -and ($maximum -notin @($ExpectedSlots,($ExpectedSlots-1)) -or $initial -ge $ExpectedSlots)) {throw 'Carousel geometry/position differs from expected wheel'}
    $identity=@{port=$Port;firmware=$version;maxPositionReply=$maximumReply;expectedSlots=$ExpectedSlots;initialPosition=$initial;baud=9600;format='8N1';dtr=$false;rts=$false}
    $identity | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'identity.json')
    $identity | ConvertTo-Json -Compress
    if($Sweep) {
        $slots=$ExpectedSlots
        # Both logical directions, wraparound, long jumps, and same-slot command.
        # Each completion is checked with an independent NOW query while idle.
        $targets=@(0..($slots-1)) + @(($slots-1)..0) + @(($slots-1),0,[int]($slots/2),0,$initial,$initial)
        $results=@()
        foreach($target in $targets) {
            $command=$target.ToString('X')
            $clock=[Diagnostics.Stopwatch]::StartNew()
            $completion=Position (Exchange $command 1 45)
            if($completion -ne $target) {throw "Move completion $completion did not match $target; no replay"}
            $actual=Position (Exchange 'NOW' 1)
            if($actual -ne $target) {throw "Readback $actual did not match $target; no replay"}
            $entry=@{target=$target;position=$actual;elapsedMs=$clock.Elapsed.TotalMilliseconds}
            $results+=$entry
            $entry | ConvertTo-Json -Compress
        }
        $results | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $OutputDirectory 'moves.json')
    }
} finally {
    # Do not blindly restore after a failed/ambiguous command. The successful
    # sweep explicitly returns to the initial position before closing.
    $serial.Dispose()
}
