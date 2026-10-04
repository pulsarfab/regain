$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'ComTestProperty.ps1')
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public sealed class ComPropertyFixture {
    public int Calls;
    public int BusyReads;
    public int ErrorCode = unchecked((int)0x8001010A);
    public string Value = "expected";
    public string Name {
        get {
            Calls++;
            if (Calls <= BusyReads) throw new COMException("fixture failure", ErrorCode);
            return Value;
        }
    }
}
'@
foreach ($code in -2147418111,-2147417846) {
    $fixture = New-Object ComPropertyFixture
    $fixture.BusyReads = 2
    $fixture.ErrorCode = $code
    if ((Get-ComTestProperty $fixture Name) -ne 'expected' -or $fixture.Calls -ne 3) { throw 'Busy read recovery failed' }
}
$fixture = New-Object ComPropertyFixture
$fixture.Value = ''
if ((Get-ComTestProperty $fixture Name) -ne '' -or $fixture.Calls -ne 1) { throw 'Empty metadata must not be retried or replaced' }
$fixture = New-Object ComPropertyFixture
$fixture.BusyReads = 100
$rejected = $false
try { Get-ComTestProperty $fixture Name -TimeoutMilliseconds 0 | Out-Null } catch { $rejected = $_.Exception.Message -match '8001010A' }
if (!$rejected -or $fixture.Calls -ne 1) { throw 'Persistent busy error must fail within deadline' }
$fixture = New-Object ComPropertyFixture
$fixture.BusyReads = 100
$fixture.ErrorCode = -2147023174 # RPC_S_SERVER_UNAVAILABLE: never retry.
$rejected = $false
try { Get-ComTestProperty $fixture Name | Out-Null } catch { $rejected = $_.Exception.Message -match '800706BA' }
if (!$rejected -or $fixture.Calls -ne 1) { throw 'Non-busy COM error was hidden/retried' }
$rejected = $false
try { Get-ComTestProperty $fixture Missing | Out-Null } catch { $rejected = $true }
if (!$rejected) { throw 'Missing property must fail' }
Write-Output 'Strict COM property reads: busy recovery, deadline, empty value and terminal errors passed.'
