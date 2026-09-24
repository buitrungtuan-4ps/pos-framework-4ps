<#
.SYNOPSIS
    Authenticode-signs Windows binaries with whatever signing this fork has configured, or says
    plainly that the release ships unsigned (ADR-0142).

.DESCRIPTION
    How a fork signs its Windows binaries is the fork's decision. This script is the one place that
    decision is read - from the environment - so the release workflow and the Station app's bundler
    call the same thing and cannot disagree.

      POS_SIGN_MODE           auto (default) | none | pfx | command
      POS_SIGN_PFX_BASE64     pfx: the .pfx file, base64-encoded: an internal certificate made by
                              new-internal-signing-cert.ps1. A public CA has not issued a .pfx
                              since June 2023; its keys stay in hardware (POS_SIGN_COMMAND)
      POS_SIGN_PFX_PASSWORD   pfx: its password
      POS_SIGN_COMMAND        command: any signer's command line, with {file} where the path goes.
                              The route for a public CA's certificate, whose key cannot leave its
                              hardware - a token, Azure Trusted Signing, a cloud KMS through jsign
      POS_SIGN_TIMESTAMP_URL  pfx: the RFC 3161 timestamp server (default http://timestamp.digicert.com).
                              A timestamp keeps the signature valid after the certificate expires
      POS_SIGN_DESCRIPTION    pfx: the description Windows shows in the UAC prompt
      POS_SIGN_REQUIRED       "true": no signing configured is a failure, not a notice. For the
                              official release; a fork without a certificate leaves it unset

    auto picks command, then pfx, then none: the first whose inputs are present.

    ORDER MATTERS. Authenticode writes the signature INTO the executable, so the bytes change. Run
    this BEFORE minisign signs the artifact for over-the-air updates. The other way round, every
    store refuses the update, because the minisign signature covers bytes that no longer exist.

    The file name is not signed. A binary signed here and then renamed for a store
    (pos-edge-setup_<cloud>_<store>.exe, ADR-0141) keeps a valid signature.

.PARAMETER Path
    One or more files to sign.

.PARAMETER Mode
    Overrides POS_SIGN_MODE.

.EXAMPLE
    ./deploy/release/sign-windows.ps1 -Path target/x86_64-pc-windows-msvc/release/pos-edge.exe
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string[]] $Path,

    [ValidateSet('auto', 'none', 'pfx', 'command')]
    [string] $Mode
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Get-Setting([string] $Name, [string] $Default = '') {
    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) { return $Default }
    return $value.Trim()
}

function Write-Outcome([string] $Text) {
    Write-Host $Text
    if ($env:GITHUB_STEP_SUMMARY) { Add-Content -Path $env:GITHUB_STEP_SUMMARY -Value "- $Text" }
}

function Find-SignTool {
    $onPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($onPath) { return $onPath.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    if (Test-Path -LiteralPath $kits) {
        $found = Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\' } |
            Sort-Object FullName -Descending |
            Select-Object -First 1
        if ($found) { return $found.FullName }
    }
    throw 'signtool.exe was not found: install the Windows SDK, or set POS_SIGN_MODE=command with your own signer'
}

if (-not $Mode) { $Mode = Get-Setting 'POS_SIGN_MODE' 'auto' }
$required = (Get-Setting 'POS_SIGN_REQUIRED' 'false') -eq 'true'
$command = Get-Setting 'POS_SIGN_COMMAND'
$pfxBase64 = Get-Setting 'POS_SIGN_PFX_BASE64'

if ($Mode -eq 'auto') {
    if ($command) { $Mode = 'command' }
    elseif ($pfxBase64) { $Mode = 'pfx' }
    else { $Mode = 'none' }
}

foreach ($file in $Path) {
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "nothing to sign at $file"
    }
}

if ($Mode -eq 'none') {
    if ($required) {
        throw 'POS_SIGN_REQUIRED is true but no signing is configured: set POS_SIGN_PFX_BASE64 or POS_SIGN_COMMAND (docs/fork-checklist.md)'
    }
    Write-Host '::notice::Windows binaries ship UNSIGNED: no POS_SIGN_PFX_BASE64 or POS_SIGN_COMMAND is configured. SmartScreen warns on the first run; over-the-air updates are unaffected, because minisign verifies those (ADR-0142).'
    Write-Outcome 'Authenticode: not signed - this fork has no certificate configured'
    return
}

if ($Mode -eq 'pfx') {
    if (-not $pfxBase64) { throw 'POS_SIGN_MODE=pfx needs POS_SIGN_PFX_BASE64' }
    $signtool = Find-SignTool
    $pfx = Join-Path ([IO.Path]::GetTempPath()) ('pos-sign-{0}.pfx' -f [Guid]::NewGuid())
    try {
        [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($pfxBase64))
        $timestamp = Get-Setting 'POS_SIGN_TIMESTAMP_URL' 'http://timestamp.digicert.com'
        $description = Get-Setting 'POS_SIGN_DESCRIPTION' "Pizza 4P's POS"
        $arguments = @('sign', '/fd', 'sha256', '/tr', $timestamp, '/td', 'sha256', '/d', $description, '/f', $pfx)
        $password = Get-Setting 'POS_SIGN_PFX_PASSWORD'
        if ($password) { $arguments += @('/p', $password) }
        $arguments += $Path
        & $signtool @arguments
        if ($LASTEXITCODE -ne 0) { throw "signtool failed with exit code $LASTEXITCODE" }
    } finally {
        # The private key exists on disk only for the length of the signtool call.
        Remove-Item -LiteralPath $pfx -Force -ErrorAction SilentlyContinue
    }
}

if ($Mode -eq 'command') {
    if (-not $command) { throw 'POS_SIGN_MODE=command needs POS_SIGN_COMMAND' }
    foreach ($file in $Path) {
        $full = (Resolve-Path -LiteralPath $file).Path
        $line = $command.Replace('{file}', '"' + $full + '"')
        & cmd.exe /d /c $line
        if ($LASTEXITCODE -ne 0) { throw "the signing command failed for $file with exit code $LASTEXITCODE" }
    }
}

# Whatever signed, prove it: each file now carries an Authenticode signature, and the log names who
# made it. A chain this runner does not trust - an internal certificate - still counts as signed;
# the store PCs that trust it are what matter.
foreach ($file in $Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $file
    $status = [string]$signature.Status
    if ($null -eq $signature.SignerCertificate -or $status -eq 'NotSigned' -or $status -eq 'HashMismatch') {
        throw "$file carries no valid Authenticode signature after signing (status $status)"
    }
    Write-Outcome ('Authenticode: {0} signed by {1} (thumbprint {2}, status {3})' -f (Split-Path -Leaf $file), $signature.SignerCertificate.Subject, $signature.SignerCertificate.Thumbprint, $status)
}
