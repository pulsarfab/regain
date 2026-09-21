# Passive serial trace: deliberately contains no write or movement commands.
param([string]$Port = 'COM3', [string]$Output = 'artifacts/eta-passive.jsonl', [int]$Frames = 8)
$ErrorActionPreference = 'Stop'
if ($Frames -lt 1 -or $Frames -gt 100) { throw 'Frames must be 1..100' }
$destination = [IO.Path]::GetFullPath($Output)
[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination)) | Out-Null
$serial = [IO.Ports.SerialPort]::new($Port,19200,[IO.Ports.Parity]::None,8,[IO.Ports.StopBits]::One)
$serial.ReadTimeout = 3000
$serial.DtrEnable = $false; $serial.RtsEnable = $false
$lines = [Collections.Generic.List[string]]::new()
try {
    $serial.Open()
    for ($i = 0; $i -lt $Frames; $i++) {
        $frame = $serial.ReadLine() + "`n"
        $lines.Add((@{utc=[DateTime]::UtcNow.ToString('O');direction='read';port=$Port;baud=19200;ascii=$frame} | ConvertTo-Json -Compress))
    }
} finally { $serial.Dispose() }
[IO.File]::WriteAllLines($destination,$lines,[Text.UTF8Encoding]::new($false))
Write-Output "Saved $($lines.Count) passive frames to $destination; no bytes written."
