#Requires -RunAsAdministrator
<#
.SYNOPSIS
    Makes this store PC trust a fork's internal code-signing certificate (ADR-0142).

.DESCRIPTION
    Imports the public .cer made by deploy/release/new-internal-signing-cert.ps1 into the machine's
    Trusted Root and Trusted Publishers stores. Afterwards the UAC prompt for pos-edge names the fork
    as its publisher. A fleet with Group Policy or Intune should push the same certificate from there
    instead; this script is for the PC that is not managed.

    The certificate can sign code and nothing else, so trusting it does not let it vouch for a
    website or issue other certificates.

.PARAMETER Certificate
    Path to pos-signing.cer.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\trust-internal-signing-cert.ps1 -Certificate .\pos-signing.cer
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $Certificate
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$imported = Import-Certificate -FilePath $Certificate -CertStoreLocation 'Cert:\LocalMachine\Root'
Import-Certificate -FilePath $Certificate -CertStoreLocation 'Cert:\LocalMachine\TrustedPublisher' | Out-Null
Write-Host "Trusted: $($imported.Subject)  thumbprint $($imported.Thumbprint)"
