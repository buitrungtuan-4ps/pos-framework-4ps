<#
.SYNOPSIS
    Makes an internal code-signing certificate, for a fork that has no certificate from a public CA
    (ADR-0142).

.DESCRIPTION
    Creates a self-signed certificate that is good for code signing and nothing else, and writes
    three files to -OutDir:

      pos-signing.pfx              the private key. Its base64 becomes the POS_SIGN_PFX_BASE64
                                   secret; then keep it offline or delete it
      pos-signing.pfx.base64.txt   that base64, ready to paste into the secret
      pos-signing.cer              the public half. It goes to every store PC - Group Policy,
                                   Intune, or deploy/edge/trust-internal-signing-cert.ps1

    What it buys: on a PC that trusts the .cer, the UAC prompt names your organisation instead of
    "Unknown publisher", and an AppLocker or WDAC publisher rule can allow the binary.
    What it does not buy: SmartScreen reputation for a file downloaded from the internet. That
    comes only from a certificate issued by a public CA, and builds up over time.

.PARAMETER Organisation
    Your organisation's name, as the UAC prompt should show it.

.PARAMETER OutDir
    Where to write the three files. Default: the current directory.

.PARAMETER Years
    How long the certificate is valid. Default: 3. A timestamped signature outlives it.

.EXAMPLE
    ./deploy/release/new-internal-signing-cert.ps1 -Organisation 'Example Pizza Co' -OutDir C:\keys
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Organisation,

    [string] $OutDir = '.',

    [ValidateRange(1, 10)]
    [int] $Years = 3
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$certificate = New-SelfSignedCertificate -Type CodeSigningCert `
    -Subject "CN=$Organisation POS code signing" `
    -CertStoreLocation 'Cert:\CurrentUser\My' `
    -KeyAlgorithm RSA -KeyLength 3072 -HashAlgorithm SHA256 `
    -KeyExportPolicy Exportable `
    -NotAfter (Get-Date).AddYears($Years)

$password = Read-Host -Prompt 'A password for the .pfx - it becomes the POS_SIGN_PFX_PASSWORD secret' -AsSecureString

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$pfx = Join-Path $OutDir 'pos-signing.pfx'
$cer = Join-Path $OutDir 'pos-signing.cer'
$base64 = Join-Path $OutDir 'pos-signing.pfx.base64.txt'

Export-PfxCertificate -Cert $certificate -FilePath $pfx -Password $password | Out-Null
Export-Certificate -Cert $certificate -FilePath $cer | Out-Null
$bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $pfx).Path)
[IO.File]::WriteAllText($base64, [Convert]::ToBase64String($bytes))

# The private key now lives in the .pfx and nowhere else on this machine.
Remove-Item -LiteralPath ('Cert:\CurrentUser\My\' + $certificate.Thumbprint) -DeleteKey

Write-Host "Certificate: $($certificate.Subject)  thumbprint $($certificate.Thumbprint)  valid until $($certificate.NotAfter)"
Write-Host "Next: paste $base64 into the POS_SIGN_PFX_BASE64 secret and the password into POS_SIGN_PFX_PASSWORD,"
Write-Host "then give $cer to every store PC. Keep the .pfx offline, or delete it once the secret is set."
