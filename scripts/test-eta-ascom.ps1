# Simulation-only fixture. Hardware movement requires the explicit hardware probe.
$ErrorActionPreference = 'Stop'
$repo = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$id = [Guid]::NewGuid().ToString()
$directory = Join-Path $repo ('artifacts/eta-com-' + $id)
New-Item -ItemType Directory -Path $directory | Out-Null
$executable = Join-Path $repo 'src/Regain.Eta.ASCOM/bin/Release/net48/Regain.Eta.ASCOM.exe'
$old = @{}
foreach ($name in 'REGAIN_ACCESSORY_SIMULATE','REGAIN_ACCESSORY_SETTINGS','REGAIN_ETA_WORKER') { $old[$name] = [Environment]::GetEnvironmentVariable($name) }
$keys = @(); $children = @(); $server = $null
$hive = [Microsoft.Win32.RegistryHive]::CurrentUser
$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if ($principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) { $hive = [Microsoft.Win32.RegistryHive]::LocalMachine }
try {
    dotnet build (Join-Path $repo 'src/Regain.Eta.ASCOM') -c Release -v quiet
    if ($LASTEXITCODE) { throw 'ETA M54 ASCOM build failed' }
    $env:REGAIN_ACCESSORY_SIMULATE = '1'
    $env:REGAIN_ACCESSORY_SETTINGS = $directory
    $env:REGAIN_ETA_WORKER = Join-Path $repo 'target/debug/regain-eta.exe'
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root = [Microsoft.Win32.RegistryKey]::OpenBaseKey($hive,$view)
        try {
            $path = 'Software\Classes\CLSID\{' + $id + '}'
            if ($root.OpenSubKey($path)) { throw 'Private test CLSID exists' }
            $keys += @{ View=$view; Path=$path }
            $key = $root.CreateSubKey($path + '\LocalServer32')
            try { $key.SetValue('', '"' + $executable + '" /test-clsid ' + $id) } finally { $key.Dispose() }
        } finally { $root.Dispose() }
    }
    # Start the isolated fixture ourselves so only it inherits simulation/profile overrides.
    $ready = Join-Path $directory 'server-ready'
    $server = Start-Process -FilePath $executable -ArgumentList @('/test-clsid',$id,'/test-ready',('"'+$ready+'"')) -WindowStyle Hidden -PassThru
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while (!(Test-Path -LiteralPath $ready)) {
        if ($server.HasExited) { throw 'COM fixture exited before registration' }
        if ([DateTime]::UtcNow -gt $deadline) { throw 'COM fixture did not become ready' }
        Start-Sleep -Milliseconds 100
    }
    foreach ($pair in @(@('System32','first'),@('SysWOW64','second'))) {
        $role=$pair[1]
        $args = '-NoProfile -ExecutionPolicy Bypass -File "' + (Join-Path $PSScriptRoot 'test-eta-ascom-client.ps1') + '" -Id ' + $id + ' -Directory "' + $directory + '" -Role ' + $role
        $children += Start-Process -FilePath "$env:WINDIR/$($pair[0])/WindowsPowerShell/v1.0/powershell.exe" -ArgumentList $args -WindowStyle Hidden -PassThru -RedirectStandardOutput (Join-Path $directory "$role.out") -RedirectStandardError (Join-Path $directory "$role.err")
    }
    foreach ($child in $children) {
        if (!$child.WaitForExit(45000)) { throw 'COM client timed out' }
        if ($child.ExitCode -ne 0) { Get-Content (Join-Path $directory '*.err'); throw 'COM client failed' }
    }
    Get-Content (Join-Path $directory '*.out')
    if (!(Test-Path (Join-Path $directory 'second-finished'))) { throw 'Shared connection test incomplete' }
    # Both clients have closed; the worker must be gone before the server idles out.
    $workers = Get-CimInstance Win32_Process -Filter "Name='regain-eta.exe'" | Where-Object ParentProcessId -eq $server.Id
    if ($workers) { throw 'Last ASCOM disconnect leaked its worker' }
    $server.Kill(); $server.WaitForExit()
    # Exercise actual SCM launch as well as an explicitly started fixture. This
    # path intentionally does not need a worker or inherit simulation variables.
    if ($hive -eq [Microsoft.Win32.RegistryHive]::LocalMachine) {
    foreach ($view in [Microsoft.Win32.RegistryView]::Registry32,[Microsoft.Win32.RegistryView]::Registry64) {
        $root=[Microsoft.Win32.RegistryKey]::OpenBaseKey($hive,$view)
        try {
            $appPath='Software\Classes\AppID\{'+$id+'}'
            $keys += @{View=$view;Path=$appPath}
            $key=$root.CreateSubKey($appPath);$key.SetValue('RunAs','Interactive User');$key.Dispose()
            $key=$root.OpenSubKey(('Software\Classes\CLSID\{'+$id+'}'),$true);$key.SetValue('AppID','{'+$id+'}');$key.Dispose()
        } finally {$root.Dispose()}
    }
    foreach ($architecture in 'System32','SysWOW64') {
        & "$env:WINDIR/$architecture/WindowsPowerShell/v1.0/powershell.exe" -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot 'test-eta-ascom-client.ps1') -Id $id -MetadataOnly
        if ($LASTEXITCODE) { throw 'Cold COM activation failed' }
    }
    Write-Output 'ETA M54 automatic COM activation passed in both architectures'
    } else { Write-Output 'Machine-wide cold activation is exercised by elevated Windows CI; local fixtures verify the running shared server.' }
} finally {
    foreach ($child in $children) { if (!$child.HasExited) { $child.Kill() }; $child.Dispose() }
    if ($server) { if (!$server.HasExited) { $server.Kill() }; $server.Dispose() }
    # Only the server launched with this randomly generated fixture CLSID.
    Get-CimInstance Win32_Process -Filter "Name='Regain.Eta.ASCOM.exe'" | Where-Object { $_.CommandLine -like "*/test-clsid $id*" } | ForEach-Object { Stop-Process -Id $_.ProcessId -ErrorAction SilentlyContinue }
    foreach ($entry in $keys) {
        $root=[Microsoft.Win32.RegistryKey]::OpenBaseKey($hive,$entry.View)
        try { $root.DeleteSubKeyTree($entry.Path,$false) } finally { $root.Dispose() }
    }
    foreach ($name in $old.Keys) { [Environment]::SetEnvironmentVariable($name,$old[$name]) }
}
