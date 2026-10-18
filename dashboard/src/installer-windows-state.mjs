// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

/**
 * The Windows installer's part of ADR-0174: one fixed path, the split layout, the move off an old
 * box's layout, and the service's own account with its state closed to every other account.
 *
 * Kept apart from `installers.mjs` because it is one decision with one record, and because each
 * piece is a block of PowerShell that `windowsBody` places at the step it belongs to. Each block is
 * written as PowerShell source, not as a JavaScript array of strings, so it reads as the script it
 * is. None of them may hold a backtick: PowerShell's escape character, and this file's template
 * delimiter.
 *
 * What only a real Windows machine can confirm is listed in gate-register row P12; the behaviour
 * here is checked against a fake Windows by `scripts/installer-behaviour.ps1`.
 *
 * @module
 */

/** @param {string} text */
function lines(text) {
  return text.replace(/^\n/u, "").replace(/\n$/u, "").split("\n");
}

/**
 * The checks that run before anything is written, and the helpers the later steps share.
 *
 * @param {string} bindPort  the listen port, or the PowerShell variable that holds it
 * @returns {string[]}
 */
export function windowsStateGuard(bindPort) {
  return lines(String.raw`
# ONE PATH, AND ONLY ONE (ADR-0174). The store's state lives at %ProgramData%\pos-edge, resolved from
# the system. -Root is still accepted so a script that passes it keeps working, but any other value
# stops here, before anything is written: this installer resets permissions, and must never do that
# to a directory somebody typed by mistake.
$fixedRoot = Join-Path $env:ProgramData 'pos-edge'
if ([System.IO.Path]::GetFullPath($Root).TrimEnd('\', '/') -ne $fixedRoot) {
    throw "refusing -Root $($Root): a store's state lives only at $fixedRoot (ADR-0174). Nothing was changed."
}
$Root = $fixedRoot
$bin = Join-Path $Root 'bin'
$data = Join-Path $Root 'data'
$logs = Join-Path $Root 'logs'
$systemSid = 'S-1-5-18'
$adminsSid = 'S-1-5-32-544'
$usersSid = 'S-1-5-32-545'

# A directory that is a link to somewhere else, or that another account made and so can change the
# permissions of, is refused the same way. Under ProgramData a standard user can create a folder, so
# this is how one planted before the install is caught.
foreach ($path in @($Root, $bin, $data, $logs)) {
    if (-not (Test-Path -LiteralPath $path)) { continue }
    if ((Get-Item -LiteralPath $path -Force).Attributes -band [System.IO.FileAttributes]::ReparsePoint) {
        throw "refusing $($path): it is a link or junction to another place. Remove it, then run this installer again. Nothing was changed."
    }
    $owner = (Get-Acl -LiteralPath $path).GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if ($owner -ne $systemSid -and $owner -ne $adminsSid) {
        throw "refusing $($path): it is owned by $owner, not by SYSTEM or Administrators, so another account could open it again. Remove it, then run this installer again. Nothing was changed."
    }
}

# Every sc.exe and icacls call is checked (ADR-0174 decision 4); this is the check.
function Assert-Exit([string] $What, [int[]] $Allow = @(0)) {
    if ($Allow -contains $LASTEXITCODE) { return $true }
    Add-Check 'FAIL' "$What failed with exit code $LASTEXITCODE"
    return $false
}

# sc.exe stop returns once SCM has sent the control, not once the edge has drained, and a start sent
# while the old process is still stopping is refused. So this waits for STOPPED, bounded.
function Stop-Edge {
    & sc.exe stop $service | Out-Null
    $null = Assert-Exit "sc.exe stop $service" @(0, 1062)
    try {
        (Get-Service -Name $service).WaitForStatus('Stopped', [TimeSpan]::FromSeconds(60))
        return $true
    } catch {
        return $false
    }
}

# SCM reports RUNNING before the edge has opened its store and bound the port; /healthz is the answer
# that counts. Bounded, and $null when nothing answered.
function Wait-Health([int] $Seconds) {
    $until = (Get-Date).AddSeconds($Seconds)
    while ((Get-Date) -lt $until) {
        try { return Invoke-RestMethod -Uri "http://127.0.0.1:${bindPort}/healthz" -TimeoutSec 2 } catch { Start-Sleep -Milliseconds 500 }
    }
    return $null
}
`);
}

/**
 * The stop, and the move off the old layout once it has stopped (ADR-0174 decision 7, steps 1 and 2). Sets
 * `$storeDir`: `data\` when the database is there, the root when it could not be moved.
 *
 * @returns {string[]}
 */
export function windowsStateMigration() {
  return lines(String.raw`
# Restart rather than start: on a re-run the service is already up on the old environment. A service
# that has not stopped is reported and started anyway, and its files are left where they are.
$stopped = $true
if ($exists -and -not (Stop-Edge)) {
    $stopped = $false
    Add-Check 'WARN' "the old $service process did not stop within 60 seconds, so the start below may be refused. If the service is not running afterwards: sc.exe start $service"
}

# THE SPLIT LAYOUT (ADR-0174). The database, its WAL files and the pairing file go in data\, which only
# the service and administrators can read; the log goes in logs\, which a standard-user till account
# can still read (ADR-0117). A box installed before kept all of it in the root, so it is moved here.
# The database moves with its WAL files or not at all: a database without its WAL loses the last
# sales written to it. A move that fails puts back what it moved, and the store keeps its old layout.
New-Item -ItemType Directory -Force -Path $data, $logs | Out-Null
$storeDir = $data
$databaseFiles = @('store.sqlite', 'store.sqlite-wal', 'store.sqlite-shm', 'store.sqlite.pre-update', 'drain-status.json')
if (Test-Path -LiteralPath (Join-Path $data 'store.sqlite')) {
    if (Test-Path -LiteralPath (Join-Path $Root 'store.sqlite')) {
        Add-Check 'WARN' "an older database is still at $Root\store.sqlite; the store uses the one in $data. Delete the old one once you are sure nothing in it is missing."
    }
} elseif (-not $stopped) {
    $storeDir = $Root
    Add-Check 'FAIL' "the database stays in $Root, because the old process had not stopped and its files are open. Run this installer again once sc.exe query $service says STOPPED."
} else {
    $moved = New-Object System.Collections.Generic.List[string]
    try {
        foreach ($name in $databaseFiles) {
            $from = Join-Path $Root $name
            if (-not (Test-Path -LiteralPath $from)) { continue }
            if (Test-Path -LiteralPath (Join-Path $data $name)) { throw "$data already has a $name" }
            Move-Item -LiteralPath $from -Destination (Join-Path $data $name)
            $moved.Add($name)
        }
    } catch {
        foreach ($name in $moved) { Move-Item -LiteralPath (Join-Path $data $name) -Destination (Join-Path $Root $name) -ErrorAction SilentlyContinue }
        $storeDir = $Root
        Add-Check 'FAIL' "could not move the database into $data ($($_.Exception.Message)), so it stays in $Root and the service stays on LocalSystem. Run this installer again once that is fixed."
    }
}
foreach ($name in @('pos-edge.log', 'pos-edge.log.1')) {
    $from = Join-Path $Root $name
    if (Test-Path -LiteralPath $from) { Move-Item -LiteralPath $from -Destination (Join-Path $logs $name) -Force -ErrorAction SilentlyContinue }
}
# The old pairing file names a code only the stopped process could redeem.
Remove-Item -LiteralPath (Join-Path $Root 'pairing-url.txt') -Force -ErrorAction SilentlyContinue
# TOML doubles the separator.
$storeToml = (Join-Path $storeDir 'store.sqlite').Replace('\', '\\')
`);
}

/**
 * The environment lines the split layout adds: where the sealed vault and its DPAPI-protected key
 * are (ADR-0174 decision 3). Only when the database is in `data\`.
 *
 * @returns {string[]}
 */
export function windowsVaultEnvironment() {
  return lines(String.raw`
# The vault: the store server seals its secrets in data\vault under a key it makes on its first start
# and protects with DPAPI for this machine (ADR-0174). Never set on the old layout, where data\ is
# not where the store's files are.
if ($storeDir -eq $data) {
    $environment += "POS_EDGE_VAULT_KEY=$(Join-Path $data 'vault.key')"
    $environment += "POS_EDGE_VAULT_DIR=$(Join-Path $data 'vault')"
}
`);
}

/**
 * The one start as LocalSystem that moves an old box's secrets out of Credential Manager (ADR-0174
 * decision 7, step 3). Sets `$harden` to whether the account may be changed afterwards.
 *
 * @returns {string[]}
 */
export function windowsFirstStart() {
  return lines(String.raw`
# ONE START AS LOCALSYSTEM, ON A BOX THAT RAN AS LOCALSYSTEM (ADR-0174 decision 7, step 3). Its
# secrets are in LocalSystem's Credential Manager, which the service's own account cannot see. On this
# start the store server makes its vault key, seals each secret into data\vault, reads it back, and
# only then deletes the Credential Manager copy. Moving the account first would leave this box
# looking as if it had never been activated.
$harden = $storeDir -eq $data
$account = $null
if ($exists) {
    try { $account = [string](Get-ItemProperty -Path $key -Name ObjectName -ErrorAction Stop).ObjectName } catch { $account = $null }
}
if ($harden -and $account -eq 'LocalSystem') {
    Write-Host 'starting once as LocalSystem, so the store server seals its secrets into data\vault'
    & sc.exe start $service | Out-Null
    $first = $null
    if (Assert-Exit "sc.exe start $service" @(0, 1056)) { $first = Wait-Health 60 }
    $null = Stop-Edge
    $firstStore = if ($null -ne $first) { $first.PSObject.Properties['store_id'] } else { $null }
    $firstLog = if (Test-Path -LiteralPath $logPath) { Get-Content -LiteralPath $logPath -Raw } else { '' }
    if ($null -eq $firstStore -or $firstStore.Value -ne $expectedStore) {
        $harden = $false
        Add-Check 'FAIL' "the store did not answer on its first start as LocalSystem, so its secrets were not moved and it stays on LocalSystem. The first error in $logPath is the cause; run this installer again once it is fixed."
    } elseif ("$firstLog" -match 'could not move a secret from the OS keyring') {
        $harden = $false
        Add-Check 'FAIL' "the store could not move every secret out of Credential Manager, so it stays on LocalSystem, where it still reads them. $logPath says which; run this installer again."
    } elseif (-not (Test-Path -LiteralPath (Join-Path $data 'vault.key'))) {
        # A release from before ADR-0174 ignores the vault settings; a vault key DPAPI would not protect
        # is the other reason. Either way the secrets stay where this account reads them.
        $harden = $false
        $environment = @($environment | Where-Object { $_ -notlike 'POS_EDGE_VAULT_*' })
        New-ItemProperty -Path $key -Name 'Environment' -PropertyType MultiString -Value $environment -Force | Out-Null
        Add-Check 'FAIL' "the store made no vault key in $data on its first start (a release older than this installer cannot), so it stays on LocalSystem with its secrets in Credential Manager. Update it, then run this installer again."
    }
}
`);
}

/**
 * The permissions, their check, the privilege and the account (ADR-0174 decisions 1, 2 and 4, and
 * step 4 of 7). Any failure leaves the service on LocalSystem.
 *
 * @returns {string[]}
 */
export function windowsStateHarden() {
  return lines(String.raw`
# THE SERVICE'S OWN ACCOUNT, AND STATE ONLY IT CAN READ (ADR-0174). Each step throws on a failure, and
# the catch puts the service back on LocalSystem, which can read everything, so the store keeps
# trading whatever went wrong here.
function Invoke-Icacls {
    $output = (& icacls.exe @args | Out-String).Trim()
    if ($LASTEXITCODE -ne 0) { throw "icacls $($args[0]) $($args[1]) failed with exit code $($LASTEXITCODE): $output" }
}

# Read back, never assumed: the ACL on $Path, against the table in ADR-0174. SYSTEM and
# Administrators may hold anything; every other account must be a reader or a writer here. A reader
# must be able to read and nothing more; a writer must be able to modify.
function Test-StateAcl([string] $Path, [bool] $Protected, [string[]] $Readers, [string[]] $Writers) {
    $acl = Get-Acl -LiteralPath $Path
    $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
    if ($owner -ne $systemSid -and $owner -ne $adminsSid) { return "$Path is owned by $owner" }
    if ($acl.AreAccessRulesProtected -ne $Protected) { return "$Path does not have inheritance $(if ($Protected) { 'off' } else { 'on' })" }
    $granted = @{}
    foreach ($rule in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
        if ([string]$rule.AccessControlType -ne 'Allow') { continue }
        $granted[$rule.IdentityReference.Value] = [int64]$granted[$rule.IdentityReference.Value] -bor ([int64]$rule.FileSystemRights -band 4294967295)
    }
    foreach ($sid in @($granted.Keys)) {
        if ($sid -eq $systemSid -or $sid -eq $adminsSid) { continue }
        if ($Readers -notcontains $sid -and $Writers -notcontains $sid) { return "$Path grants access to $sid" }
        # Write, append, delete, change permissions, take ownership, and the generic write and all.
        if ($Writers -notcontains $sid -and ($granted[$sid] -band 0x500D0156)) { return "$Path lets $sid write" }
    }
    foreach ($sid in $Readers) { if (-not ([int64]$granted[$sid] -band 1)) { return "$Path does not let $sid read" } }
    foreach ($sid in $Writers) { if (([int64]$granted[$sid] -band 0x301BF) -ne 0x301BF) { return "$Path does not let $sid modify" } }
    return $null
}

# SeCreateSymbolicLinkPrivilege, granted in local security policy: the edge retargets bin\current
# with a symbolic link on an update (ADR-0055), and the service's account holds no privilege unless
# it was granted. Added beside whoever holds it already, never in place of them.
function Grant-SymbolicLinkRight([string] $Sid) {
    $work = Join-Path ([System.IO.Path]::GetTempPath()) ('pos-edge-rights-' + [guid]::NewGuid())
    New-Item -ItemType Directory -Path $work | Out-Null
    try {
        $exported = Join-Path $work 'exported.inf'
        & secedit.exe /export /cfg $exported /areas USER_RIGHTS /quiet | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /export failed with exit code $LASTEXITCODE" }
        $line = @(Get-Content -LiteralPath $exported | Where-Object { $_ -match '^SeCreateSymbolicLinkPrivilege\s*=' }) | Select-Object -First 1
        $holders = @()
        if ($line) { $holders = @(($line -split '=', 2)[1].Split(',') | ForEach-Object { $_.Trim() } | Where-Object { $_ }) }
        if ($holders -contains "*$Sid") { return }
        $holders += "*$Sid"
        $policy = Join-Path $work 'grant.inf'
        Set-Content -LiteralPath $policy -Encoding Unicode -Value @('[Unicode]', 'Unicode=yes', '[Version]', 'signature="$CHICAGO$"', 'Revision=1', '[Privilege Rights]', "SeCreateSymbolicLinkPrivilege = $($holders -join ',')")
        & secedit.exe /configure /db (Join-Path $work 'grant.sdb') /cfg $policy /areas USER_RIGHTS /quiet | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "secedit /configure failed with exit code $LASTEXITCODE" }
    } finally {
        Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($harden) {
    try {
        if (-not (Stop-Edge)) { throw "$service did not stop within 60 seconds" }
        & sc.exe sidtype $service unrestricted | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "sc.exe sidtype $service failed with exit code $LASTEXITCODE" }
        $sidText = (& sc.exe showsid $service | Out-String)
        if ($LASTEXITCODE -ne 0 -or $sidText -notmatch '(S-1-5-80(-\d+){5})') { throw "sc.exe showsid $service named no service SID" }
        $serviceSid = $Matches[1]

        # Owners first, so no file a standard user planted before this run keeps the owner's right to
        # change its permissions; then every child back to inheriting; then the four directories.
        # /L acts on bin\current itself, never on the file it points at.
        Invoke-Icacls $Root /setowner "*$adminsSid" /T /C /L /Q
        Invoke-Icacls (Join-Path $Root '*') /reset /T /C /L /Q
        Invoke-Icacls $Root /inheritance:r /grant:r "*$($systemSid):(OI)(CI)F" "*$($adminsSid):(OI)(CI)F" "*$($serviceSid):(OI)(CI)RX" "*$($usersSid):(OI)(CI)RX" /Q
        Invoke-Icacls $bin /grant:r "*$($serviceSid):(OI)(CI)M" /Q
        Invoke-Icacls $logs /grant:r "*$($serviceSid):(OI)(CI)M" /Q
        Invoke-Icacls $data /inheritance:r /grant:r "*$($systemSid):(OI)(CI)F" "*$($adminsSid):(OI)(CI)F" "*$($serviceSid):(OI)(CI)M" /Q

        # An array even when empty, which Set-StrictMode needs for .Count.
        $wrong = @(@(
            Test-StateAcl $Root $true @($serviceSid, $usersSid) @()
            Test-StateAcl $bin $false @($usersSid) @($serviceSid)
            Test-StateAcl $logs $false @($usersSid) @($serviceSid)
            Test-StateAcl $data $true @() @($serviceSid)
            Get-ChildItem -LiteralPath $data -Recurse -Force | ForEach-Object { Test-StateAcl $_.FullName $false @() @($serviceSid) }
        ) | Where-Object { $_ })
        if ($wrong.Count -gt 0) { throw "the permissions did not come out as ADR-0174 sets them: $($wrong -join '; ')" }

        Grant-SymbolicLinkRight $serviceSid
        & sc.exe config $service obj= "NT SERVICE\$service" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "sc.exe config $service obj= NT SERVICE\$service failed with exit code $LASTEXITCODE" }
        & sc.exe privs $service SeChangeNotifyPrivilege/SeCreateSymbolicLinkPrivilege | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "sc.exe privs $service failed with exit code $LASTEXITCODE" }
        Add-Check 'ok' "pos-edge runs as NT SERVICE\$service, and only it and administrators can read $data"
    } catch {
        & sc.exe config $service obj= LocalSystem | Out-Null
        $back = if ($LASTEXITCODE -eq 0) { 'The service stays on LocalSystem, which can still read everything, so the store keeps trading' } else { "Putting the service back on LocalSystem failed too (exit code $LASTEXITCODE): run sc.exe config $service obj= LocalSystem" }
        Add-Check 'FAIL' "$($_.Exception.Message). $back. Run this installer again once that is fixed (ADR-0174)."
    }
}
`);
}
