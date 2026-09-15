# Inno invokes this for both the uninstaller and the final setup executable.
param([Parameter(Mandatory)][string]$Path)
$ErrorActionPreference = 'Stop'
foreach ($name in 'SIGNING_ENDPOINT','SIGNING_ACCOUNT','SIGNING_PROFILE') {
    if (![Environment]::GetEnvironmentVariable($name)) { throw "Missing $name" }
}
Import-Module ArtifactSigning -RequiredVersion 0.1.8
Invoke-ArtifactSigning -Endpoint $env:SIGNING_ENDPOINT -CodeSigningAccountName $env:SIGNING_ACCOUNT -CertificateProfileName $env:SIGNING_PROFILE -Files $Path -FileDigest SHA256 -TimestampRfc3161 'http://timestamp.acs.microsoft.com' -TimestampDigest SHA256
$signature = Get-AuthenticodeSignature -LiteralPath $Path
if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'StackFoundry LLC') { throw "Invalid signature: $Path" }
