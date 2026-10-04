# Test-client reads must not use PowerShell's property adapter: it can turn
# getter exceptions into $null. Reflection preserves the underlying COM error.
function Get-ComTestProperty {
    param(
        [Parameter(Mandatory)][object]$Device,
        [Parameter(Mandatory)][string]$Name,
        [int]$TimeoutMilliseconds = 5000
    )
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        try {
            return $Device.GetType().InvokeMember($Name, [Reflection.BindingFlags]::GetProperty, $null, $Device, $null)
        } catch {
            $cause = $_.Exception
            while ($cause.InnerException) { $cause = $cause.InnerException }
            # Only read-only calls rejected by a busy COM apartment are retried.
            # Invalid names, disconnected servers and actual getter errors fail.
            $code = $cause.HResult
            $busy = $cause -is [Runtime.InteropServices.COMException] -and
                ($code -eq -2147418111 -or $code -eq -2147417846) # 80010001 / 8001010A
            if (!$busy -or $elapsed.ElapsedMilliseconds -ge $TimeoutMilliseconds) {
                throw "COM property '$Name' failed (HRESULT 0x$($code.ToString('X8'))): $($cause.Message)"
            }
            Write-Warning "COM property '$Name' temporarily busy (HRESULT 0x$($code.ToString('X8'))); retrying within the bounded metadata deadline."
            Start-Sleep -Milliseconds 100
        }
    }
}
