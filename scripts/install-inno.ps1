# Pinned build tool, used by CI and optionally for local builds.
$ErrorActionPreference = 'Stop'
$tools = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../artifacts/tools'))
New-Item -ItemType Directory -Path $tools -Force | Out-Null
$download = Join-Path $tools 'innosetup-6.7.3.exe'
Invoke-WebRequest 'https://github.com/jrsoftware/issrc/releases/download/is-6_7_3/innosetup-6.7.3.exe' -OutFile $download
if ((Get-FileHash -LiteralPath $download).Hash -ne '9c73c3bae7ed48d44112a0f48e66742c00090bdb5bef71d9d3c056c66e97b732') { throw 'Inno Setup download hash mismatch' }
$signature = Get-AuthenticodeSignature -LiteralPath $download
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'Pyrsys B.V.') { throw 'Invalid Inno Setup signature' }
$compiler = Start-Process -FilePath $download -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/CURRENTUSER',('/DIR="' + (Join-Path $tools 'inno') + '"') -WindowStyle Hidden -Wait -PassThru
if ($compiler.ExitCode) { throw 'Inno Setup installation failed' }
