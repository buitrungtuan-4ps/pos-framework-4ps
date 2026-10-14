# Runs the generated Windows installer against a fake Windows, one scenario per failure a store
# technician can meet, and checks what the installer tells them.
#
# WHY THIS EXISTS. The parse gate (installer-syntax.mjs, and the Windows CI job) proves the script
# is PowerShell. It cannot prove the script *does* anything right, and the defects that reached a
# real shop PC were all behaviour: a re-install printed the previous process's pairing code, opened
# the setup page before the store was listening, and printed a pairing path with no address a
# device could open. So this runs the real template, with the Windows-only commands it calls
# (sc.exe, the service controller, the firewall, the network cmdlets, the two HTTP probes) replaced
# by functions that play each scenario, and asserts on the summary the installer prints.
#
# It runs under PowerShell 7 on any OS: nothing here needs Windows. installer-syntax.mjs runs it
# whenever `pwsh` is on the PATH, which every GitHub-hosted runner has.
#
#   pwsh -NoProfile -File dashboard/scripts/installer-behaviour.ps1 -Script deploy/edge/install-pos-edge.ps1

param(
    [Parameter(Mandatory = $true)]
    [string] $Script
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$Script = (Resolve-Path -LiteralPath $Script).Path
# The template refuses to run unelevated, which is right on Windows and meaningless here.
$body = (Get-Content -LiteralPath $Script -Raw) -replace '(?m)^#Requires -RunAsAdministrator\s*$', ''
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("pos-installer-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $work | Out-Null
$installer = Join-Path $work 'install.ps1'
[System.IO.File]::WriteAllText($installer, $body, (New-Object System.Text.UTF8Encoding $true))
$binary = Join-Path $work 'pos-edge.exe'
# The binary the installer carries, which it runs as `pos-edge.exe promote` from its rescue copy when a
# re-run finds an older release. FAKE_PROMOTE says how that goes: 'ok' commits as the edge would,
# leaving the unconfirmed marker the fake SCM reads; 'self-test' refuses as the edge does.
[System.IO.File]::WriteAllText($binary, @'
#!/bin/sh
root=$(dirname "$POS_EDGE_CONFIG")
echo "promote $*" >> "$root/promote-calls"
case "$FAKE_PROMOTE" in
  ok) echo 0 > "$root/bin/unconfirmed"; echo "installed in place of the previous release"; exit 0 ;;
  *) echo 'Error: Install("pos-edge 0.14.1 failed its self-test on this PC, so the release already installed was kept; run pos-edge.exe --self-test to see why")' >&2; exit 1 ;;
esac
'@)
& chmod +x $binary

$store = '01M2MQ2BH6PKH6W4SEN2YVP9VT'
$script:failures = 0

# ---------------------------------------------------------------------------------------------
# The fake Windows. Every function reads $global:scene, which each scenario sets.
# ---------------------------------------------------------------------------------------------

function global:sc.exe {
    $verb = $args[0]
    if ($global:scene.ScFails -eq $verb) { $global:LASTEXITCODE = 5; return "[SC] $verb FAILED 5:" }
    switch ($verb) {
        'start' {
            $global:scene.Events.Add('start')
            if ($global:scene.StartCode -ne 0) {
                $global:LASTEXITCODE = $global:scene.StartCode
                return "[SC] StartService FAILED $($global:scene.StartCode):"
            }
            # SCM answers at once; the edge itself takes a moment, and writes its pairing file and binds
            # the port only on the installer's first wait (see Start-Sleep). An edge that wrote it here
            # would hide the race a real re-install hits.
            $global:scene.Started = $true
            # A promote that committed left the marker: the service now starts the carried release.
            if (Test-Path -LiteralPath (Join-Path $global:scene.Root 'bin/unconfirmed')) {
                $global:scene.Running = $global:scene.Carried
            }
            # What a release that knows ADR-0174 does with the vault settings on its first start.
            if ($global:scene.MakesVaultKey -and (Test-Path -LiteralPath (Join-Path $global:scene.Root 'data'))) {
                Set-Content -LiteralPath (Join-Path $global:scene.Root 'data/vault.key') -Value 'protected'
            }
            if ($null -ne $global:scene.FirstLog -and (Test-Path -LiteralPath (Join-Path $global:scene.Root 'logs'))) {
                Set-Content -LiteralPath (Join-Path $global:scene.Root 'logs/pos-edge.log') -Value $global:scene.FirstLog
            }
            $global:LASTEXITCODE = 0
            return '[SC] StartService SUCCESS'
        }
        'stop' {
            $global:scene.Stopped = $true; $global:scene.Started = $false; $global:scene.Listening = $false
            $global:LASTEXITCODE = 0; return '[SC] ControlService SUCCESS'
        }
        'create' { $global:scene.Registered = $true; $global:LASTEXITCODE = 0; return '[SC] CreateService SUCCESS' }
        'showsid' { $global:LASTEXITCODE = 0; return "NAME: pos-edge`nSERVICE SID: $($global:scene.ServiceSid)`nSTATUS: Inactive" }
        'config' {
            $at = [array]::IndexOf($args, 'obj=')
            if ($at -ge 0) { $global:scene.Events.Add("account $($args[$at + 1])") }
            $global:LASTEXITCODE = 0; return '[SC] ChangeServiceConfig SUCCESS'
        }
        'privs' { $global:scene.Events.Add("privs $($args[2])"); $global:LASTEXITCODE = 0; return '[SC] ChangeServiceConfig2 SUCCESS' }
        'query' { $global:LASTEXITCODE = 0; return 'SERVICE_NAME: pos-edge' }
        default { $global:LASTEXITCODE = 0; return "[SC] $verb SUCCESS" }
    }
}

function global:Get-Service {
    param([string] $Name, $ErrorAction)
    if (-not $global:scene.Registered) { return $null }
    $service = [pscustomobject]@{ Status = $(if ($global:scene.Started) { 'Running' } else { 'Stopped' }) }
    $service | Add-Member -MemberType ScriptMethod -Name WaitForStatus -Value {
        if ($global:scene.StopHangs) { throw [System.TimeoutException]::new('Time out has expired') }
    }
    return $service
}

function global:New-ItemProperty { }

# The account the service runs as now, from its registry key.
function global:Get-ItemProperty {
    param($Path, $Name, $ErrorAction)
    if ($null -eq $global:scene.Account) { throw 'no such property' }
    return [pscustomobject]@{ ObjectName = $global:scene.Account }
}

# Windows' permissions, as ADR-0174 sets them once icacls has run; a scenario can make one come out
# wrong, or give a directory another owner. Rights are the FileSystemRights bits: full, modify, read.
function global:Get-Acl {
    param([string] $LiteralPath)
    $relative = [System.IO.Path]::GetRelativePath($global:scene.Root, [System.IO.Path]::GetFullPath($LiteralPath))
    $top = ($relative -split '[\\/]')[0]
    $service = $global:scene.ServiceSid
    $grants = @{ 'S-1-5-18' = 0x1F01FF; 'S-1-5-32-544' = 0x1F01FF }
    $protected = $false
    if ($relative -eq '.') {
        $protected = $true; $grants[$service] = 0x1200A9; $grants['S-1-5-32-545'] = 0x1200A9
    } elseif ($top -eq 'data') {
        $protected = $relative -eq 'data'; $grants[$service] = 0x301BF
        if ($global:scene.AclDrift) { $grants['S-1-5-11'] = 0x1200A9 }
    } else {
        $grants[$service] = 0x301BF; $grants['S-1-5-32-545'] = 0x1200A9
    }
    $owner = if ($global:scene.ForeignOwner -eq $relative) { 'S-1-5-21-1-2-3-1001' } else { 'S-1-5-32-544' }
    $rules = @(foreach ($sid in $grants.Keys) {
        [pscustomobject]@{ AccessControlType = 'Allow'; IdentityReference = [pscustomobject]@{ Value = $sid }; FileSystemRights = $grants[$sid] }
    })
    $acl = [pscustomobject]@{ AreAccessRulesProtected = $protected; OwnerSid = $owner; Rules = $rules }
    $acl | Add-Member -MemberType ScriptMethod -Name GetOwner -Value { param($type) [pscustomobject]@{ Value = $this.OwnerSid } }
    $acl | Add-Member -MemberType ScriptMethod -Name GetAccessRules -Value { param($explicit, $inherited, $type) $this.Rules }
    return $acl
}

function global:icacls.exe {
    $global:scene.Icacls.Add(($args -join ' '))
    if ($global:scene.IcaclsFails) { $global:LASTEXITCODE = 5; return 'Access is denied.' }
    $global:LASTEXITCODE = 0
    return 'Successfully processed 1 files; Failed processing 0 files'
}

function global:secedit.exe {
    $cfg = $args[[array]::IndexOf($args, '/cfg') + 1]
    if ($args[0] -eq '/export') {
        Set-Content -LiteralPath $cfg -Value @('[Privilege Rights]', 'SeCreateSymbolicLinkPrivilege = *S-1-5-32-544')
    } else {
        $global:scene.Events.Add('secedit ' + ((Get-Content -LiteralPath $cfg) -join ' '))
    }
    $global:LASTEXITCODE = 0
}
function global:Get-NetFirewallRule { @() }
function global:Remove-NetFirewallRule { }
function global:New-NetFirewallRule { }

function global:Get-NetTCPConnection {
    param($State, $LocalPort, $ErrorAction)
    if ($null -ne $global:scene.PortHolder) {
        return [pscustomobject]@{ OwningProcess = $global:scene.PortHolder }
    }
}

function global:Get-NetConnectionProfile {
    param($ErrorAction)
    [pscustomobject]@{ Name = 'Shop'; InterfaceAlias = 'Ethernet'; InterfaceIndex = 7; NetworkCategory = $global:scene.Network }
    # A VPN adapter, which has a connection profile of its own and no default gateway.
    if ($null -ne $global:scene.Vpn) {
        [pscustomobject]@{ Name = 'OpenVPN TAP-Windows6'; InterfaceAlias = 'OpenVPN TAP-Windows6'; InterfaceIndex = 19; NetworkCategory = $global:scene.Vpn }
    }
}

function global:Get-NetIPConfiguration {
    foreach ($address in $global:scene.Lan) {
        [pscustomobject]@{
            InterfaceIndex     = 7
            IPv4DefaultGateway = [pscustomobject]@{ NextHop = '192.168.1.1' }
            IPv4Address        = [pscustomobject]@{ IPAddress = $address }
            NetAdapter         = [pscustomobject]@{ Status = 'Up' }
        }
    }
    # A virtual switch: no gateway, so it must never be offered to a till.
    [pscustomobject]@{ InterfaceIndex = 9; IPv4DefaultGateway = $null; IPv4Address = [pscustomobject]@{ IPAddress = '172.20.0.1' }; NetAdapter = $null }
    # The VPN adapter: up, but with no gateway of its own.
    [pscustomobject]@{ InterfaceIndex = 19; IPv4DefaultGateway = $null; IPv4Address = [pscustomobject]@{ IPAddress = '10.8.0.6' }; NetAdapter = [pscustomobject]@{ Status = 'Up' } }
}

function global:Invoke-RestMethod {
    param([string] $Uri, $TimeoutSec)
    # The process a re-run finds, answering until the installer stops it.
    if ($global:scene.AnswersBefore -and -not $global:scene.Stopped) {
        $global:scene.Events.Add('probe')
        return [pscustomobject]@{ status = 'ok'; version = $global:scene.Running; store_id = $global:scene.AnsweringStore }
    }
    if (-not $global:scene.Listening -or -not $global:scene.Answers) { throw 'connection refused' }
    $global:scene.Events.Add('health')
    $health = [ordered]@{ status = 'ok'; store_id = $global:scene.AnsweringStore }
    # $null: an answer with no version in it, which the installer reports as 'unknown'.
    if ($null -ne $global:scene.Running) { $health.version = $global:scene.Running }
    return [pscustomobject]$health
}

function global:Invoke-WebRequest {
    param([string] $Uri, [switch] $UseBasicParsing, $TimeoutSec)
    $date = (Get-Date).ToUniversalTime().AddMinutes($global:scene.ClockSkewMinutes).ToString('R')
    switch ($global:scene.Cloud) {
        'ok' { return [pscustomobject]@{ StatusCode = 200; Headers = @{ Date = $date } } }
        'http-error' {
            # Windows PowerShell 5.1's shape: the response's headers index by name.
            $exception = [System.Exception]::new('The remote server returned an error: (404) Not Found.')
            $exception | Add-Member -NotePropertyName Response -NotePropertyValue ([pscustomobject]@{ Headers = @{ Date = $date } })
            throw $exception
        }
        'http-error-7' {
            # PowerShell 7's shape: an HttpResponseMessage, whose headers cannot be indexed by name.
            $exception = [System.Exception]::new('Response status code does not indicate success: 404 (Not Found).')
            $exception | Add-Member -NotePropertyName Response -NotePropertyValue ([pscustomobject]@{ Headers = [pscustomobject]@{ Date = $date } })
            throw $exception
        }
        default { throw 'No such host is known.' }
    }
}

function global:Start-Process { param($FilePath) $global:scene.Events.Add("browser $FilePath") }

# The installer waits in bounded loops. A clock that each sleep moves forward keeps a scenario where
# nothing ever answers from taking the whole thirty seconds for real.
function global:Start-Sleep {
    param($Milliseconds, $Seconds)
    $global:scene.Clock = $global:scene.Clock.AddMilliseconds([double]$Milliseconds)
    # The edge, some time after its start: it mints a code, writes the file, then listens.
    if ($global:scene.Started -and -not $global:scene.Listening) {
        if ($null -ne $global:scene.Pairing) {
            Set-Content -LiteralPath $global:scene.PairingPath -Value $global:scene.Pairing
        }
        $global:scene.Listening = $true
    }
}
function global:Get-Date { $global:scene.Clock }

# ---------------------------------------------------------------------------------------------
# The scenarios.
# ---------------------------------------------------------------------------------------------

function Invoke-Scenario {
    param([string] $Name, [hashtable] $Overrides)
    # The installer takes its root from %ProgramData% (ADR-0174), so each scenario has its own.
    $env:ProgramData = Join-Path $work ($Name -replace '[^A-Za-z0-9]', '-')
    $root = Join-Path $env:ProgramData 'pos-edge'
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    $global:scene = @{
        Root = $root; RootArg = $root; Account = 'LocalSystem'; ServiceSid = 'S-1-5-80-1-2-3-4-5'
        MakesVaultKey = $true; FirstLog = $null; AclDrift = $false; ForeignOwner = $null; IcaclsFails = $false
        ScFails = $null; OldLayout = $true; Icacls = [System.Collections.Generic.List[string]]::new()
        Registered = $true; Started = $false; Listening = $false; StartCode = 0; StopHangs = $false
        Pairing = '/pair?code=222222'; PairingPath = (Join-Path $root 'data/pairing-url.txt')
        Answers = $true; AnsweringStore = $store; PortHolder = $null
        Network = 'Private'; Vpn = $null; Lan = @('192.168.1.20'); Cloud = 'ok'; ClockSkewMinutes = 0
        Running = '0.14.0'; Carried = ''; AnswersBefore = $false; Stopped = $false; Promote = 'ok'
        Clock = [DateTime]::Now; Events = [System.Collections.Generic.List[string]]::new()
        Stale = $true; Kept = $true; Log = $null
    }
    foreach ($key in $Overrides.Keys) { $global:scene[$key] = $Overrides[$key] }
    # What a PC that ran the installer before looks like: the slot layout, and the pairing file the
    # previous process wrote and nothing deleted.
    if ($global:scene.Kept) {
        New-Item -ItemType Directory -Path (Join-Path $root 'bin') -Force | Out-Null
        Set-Content -LiteralPath (Join-Path $root 'bin/current') -Value 'the binary the edge installed'
    }
    # The layout before ADR-0174: the database, its WAL and the last run's pairing file in the root.
    if ($global:scene.Kept -and $global:scene.OldLayout) {
        Set-Content -LiteralPath (Join-Path $root 'store.sqlite') -Value 'the database'
        Set-Content -LiteralPath (Join-Path $root 'store.sqlite-wal') -Value 'the last sales'
        if ($global:scene.Stale) { Set-Content -LiteralPath (Join-Path $root 'pairing-url.txt') -Value '/pair?code=111111' }
    } elseif ($global:scene.Stale) {
        New-Item -ItemType Directory -Path (Join-Path $root 'data') -Force | Out-Null
        Set-Content -LiteralPath $global:scene.PairingPath -Value '/pair?code=111111'
    }
    if ($null -ne $global:scene.Log) { Set-Content -LiteralPath (Join-Path $root 'pos-edge.log') -Value $global:scene.Log }

    # A script that stops half-way is the worst outcome for a technician, so a scenario that ends in
    # an exception is kept as text for the assertions to fail on, not left to end this run.
    $env:FAKE_PROMOTE = $global:scene.Promote
    try {
        $output = & $installer -Binary $binary -StoreId $store -CloudUrl 'https://cloud.example.com' -Root $global:scene.RootArg -OpenSetup -CarriedVersion $global:scene.Carried 6>&1 3>&1 2>&1 |
            ForEach-Object { "$_" }
    } catch {
        $output = @("THE INSTALLER STOPPED: $($_.Exception.Message)")
    }
    return [pscustomobject]@{
        Name = $Name; Text = ($output -join "`n"); Events = @($global:scene.Events); Root = $root
        Icacls = @($global:scene.Icacls)
        Promoted = Test-Path -LiteralPath (Join-Path $root 'promote-calls')
        Unconfirmed = Test-Path -LiteralPath (Join-Path $root 'bin/unconfirmed')
    }
}

function Assert-Says($result, [string] $expected) {
    if ($result.Text.Contains($expected)) {
        Write-Host "ok    $($result.Name): says `"$expected`""
    } else {
        Write-Host "FAIL  $($result.Name): does not say `"$expected`""
        Write-Host (($result.Text -split "`n" | ForEach-Object { "      $_" }) -join "`n")
        $script:failures++
    }
}

function Assert-Silent($result, [string] $unexpected) {
    if ($result.Text.Contains($unexpected)) {
        Write-Host "FAIL  $($result.Name): should not say `"$unexpected`""
        Write-Host (($result.Text -split "`n" | ForEach-Object { "      $_" }) -join "`n")
        $script:failures++
    } else {
        Write-Host "ok    $($result.Name): silent on `"$unexpected`""
    }
}

function Assert-True($result, [bool] $condition, [string] $what) {
    if ($condition) { Write-Host "ok    $($result.Name): $what" } else { Write-Host "FAIL  $($result.Name): $what"; $script:failures++ }
}

# A. The owner's PC: installed before, a pairing file left behind, a path-only pairing URL.
$r = Invoke-Scenario 'A re-install' @{}
Assert-Says $r 'http://192.168.1.20:8787/pair?code=222222'
Assert-Silent $r 'code=111111'
Assert-Silent $r '172.20.0.1'
Assert-Says $r 'ok    pos-edge 0.14.0 is answering on port 8787 for store 01M2MQ2BH6PKH6W4SEN2YVP9VT'
Assert-Says $r 'this PC already had pos-edge, so the binary it runs was kept (version 0.14.0)'
Assert-Says $r 'ok    the cloud answers at https://cloud.example.com'
Assert-Silent $r 'FAIL'
Assert-Silent $r 'WARN'
Assert-Silent $r 'THE INSTALLER STOPPED'
$browser = [array]::IndexOf($r.Events, 'browser http://localhost:8787/setup')
$health = [array]::IndexOf($r.Events, 'health')
Assert-True $r ($health -ge 0 -and $browser -gt $health) 'opens the setup page only after /healthz answered'

# B. A first install on a clean PC: no note about a kept binary.
$r = Invoke-Scenario 'B first install' @{ Kept = $false; Stale = $false; Registered = $false }
Assert-Silent $r 'already had pos-edge'
Assert-Says $r 'http://192.168.1.20:8787/pair?code=222222'

# C. Something else holds the port.
$r = Invoke-Scenario 'C port taken' @{ PortHolder = $PID; Answers = $false; Log = "ERROR pos_edge: could not bind 0.0.0.0:8787: address in use" }
Assert-Says $r 'FAIL  port 8787 is already in use by'
Assert-Says $r "(PID $PID)"

# D. The store never answers: the log is shown, and the service state.
$r = Invoke-Scenario 'D never answers' @{ Answers = $false; Pairing = $null; Log = "ERROR pos_edge: could not open the store database" }
Assert-Says $r 'FAIL  nothing answered on port 8787 within 30 seconds'
Assert-Says $r 'could not open the store database'
Assert-Says $r 'WARN  no pairing code was written within 30 seconds'
Assert-Silent $r 'browser'

# E. Another store's edge answers on the port.
$r = Invoke-Scenario 'E another store' @{ AnsweringStore = '01JBQ9ZK7X8N4M2P6R3T5V7W9Y' }
Assert-Says $r 'FAIL  port 8787 is answered by pos-edge 0.14.0 for store 01JBQ9ZK7X8N4M2P6R3T5V7W9Y'

# F. The network is Public, where the firewall rule does not apply.
$r = Invoke-Scenario 'F public network' @{ Network = 'Public' }
Assert-Says $r "WARN  network 'Shop' (Ethernet) is Public"
Assert-Says $r 'Set-NetConnectionProfile -InterfaceIndex 7 -NetworkCategory Private'

# F2. The owner's PC: the Wi-Fi and an OpenVPN adapter both Public. Only the LAN is worth a line; a
#     VPN on Public is how it should be.
$r = Invoke-Scenario 'F2 public LAN and public VPN' @{ Network = 'Public'; Vpn = 'Public' }
Assert-Says $r "WARN  network 'Shop' (Ethernet) is Public"
Assert-Silent $r 'OpenVPN'
Assert-Silent $r '10.8.0.6'
$r = Invoke-Scenario 'F3 private LAN, public VPN' @{ Vpn = 'Public' }
Assert-Silent $r 'WARN'

# M. A re-run keeps the release that is running; the summary names both, and which way to go.
# The old process was not answering when the installer started, so its release was not known and
# nothing was put in place; it answers after the restart, so a second run can.
$r = Invoke-Scenario 'M older release running' @{ Running = '0.11.0'; Carried = '0.14.0' }
Assert-Says $r 'runs 0.11.0, which was kept because it was not answering when this installer started'
Assert-Says $r 'Run this installer again to put 0.14.0 in place'
$r = Invoke-Scenario 'M newer release running' @{ Running = '0.14.0'; Carried = '0.13.0' }
Assert-Says $r 'runs 0.14.0, newer than the 0.13.0 this installer carries, so it was kept.'
Assert-Silent $r 'To run 0.13.0'
$r = Invoke-Scenario 'M same release' @{ Running = '0.14.0'; Carried = '0.14.0' }
Assert-Says $r 'the binary it runs was kept (version 0.14.0)'
$r = Invoke-Scenario 'M running release unknown' @{ Running = $null; Carried = '0.14.0' }
Assert-Says $r 'the binary it runs was kept (version unknown)'
Assert-Silent $r 'newer than'

# N. A re-run carrying a newer release than the one answering puts it in place, through the edge's own
#    update steps, and never an older or equal one (ADR-0140 Amendment 1).
$r = Invoke-Scenario 'N newer release carried' @{ AnswersBefore = $true; Running = '0.11.0'; Carried = '0.14.1' }
Assert-Says $r 'ok    upgraded from 0.11.0 to 0.14.1'
Assert-Says $r 'ok    pos-edge 0.14.1 is answering on port 8787'
Assert-Silent $r 'which was kept'
Assert-True $r (-not $r.Unconfirmed) 'ends the trial once the new release answers'
$r = Invoke-Scenario 'N promote fails its self-test' @{ AnswersBefore = $true; Running = '0.11.0'; Carried = '0.14.1'; Promote = 'self-test' }
Assert-Says $r 'WARN  could not put 0.14.1 in place of 0.11.0, so this PC still runs 0.11.0'
Assert-Says $r 'failed its self-test on this PC'
Assert-Says $r 'ok    pos-edge 0.11.0 is answering on port 8787'
Assert-Says $r "To run 0.14.1 here, roll it out from the console's OTA screen."
$r = Invoke-Scenario 'N older release carried' @{ AnswersBefore = $true; Running = '0.14.1'; Carried = '0.14.0' }
Assert-True $r (-not $r.Promoted) 'never puts an older release in place'
Assert-Says $r 'runs 0.14.1, newer than the 0.14.0 this installer carries, so it was kept.'
$r = Invoke-Scenario 'N same release carried' @{ AnswersBefore = $true; Running = '0.14.1'; Carried = '0.14.1' }
Assert-True $r (-not $r.Promoted) 'does not reinstall the release that is running'
$r = Invoke-Scenario 'N promoted release never answers' @{ AnswersBefore = $true; Running = '0.11.0'; Carried = '0.14.1'; Answers = $false }
Assert-Says $r 'FAIL  nothing answered on port 8787 within 30 seconds'
Assert-Says $r 'is on trial: if it cannot start three times, the service goes back to 0.11.0 by itself'
Assert-True $r $r.Unconfirmed 'leaves the trial running, so the old release comes back by itself'
$r = Invoke-Scenario 'N another store answers first' @{ AnswersBefore = $true; Running = '0.11.0'; Carried = '0.14.1'; AnsweringStore = '01JBQ9ZK7X8N4M2P6R3T5V7W9Y' }
Assert-True $r (-not $r.Promoted) 'takes no other store''s release for this one'

# G. The cloud cannot be reached; then it answers with an error status, which still proves the network.
$r = Invoke-Scenario 'G no cloud' @{ Cloud = 'down' }
Assert-Says $r 'FAIL  cannot reach the cloud at https://cloud.example.com (No such host is known.)'
$r = Invoke-Scenario 'G cloud answers 404' @{ Cloud = 'http-error' }
Assert-Says $r 'ok    the cloud answers at https://cloud.example.com'
$r = Invoke-Scenario 'G cloud answers 404 under PowerShell 7' @{ Cloud = 'http-error-7' }
Assert-Says $r 'ok    the cloud answers at https://cloud.example.com'
Assert-Says $r 'pos-edge setup summary'

# H. The clock is ten minutes out.
$r = Invoke-Scenario 'H clock skew' @{ ClockSkewMinutes = 10 }
Assert-Says $r "WARN  this PC's clock is 10 minutes away from the cloud's"

# I. No adapter with a gateway: a placeholder a technician can fill, and a warning.
$r = Invoke-Scenario 'I no LAN' @{ Lan = @() }
Assert-Says $r "http://<this PC's IPv4 address, from ipconfig>:8787/pair?code=222222"
Assert-Says $r 'WARN  no network adapter with a default gateway is up'
# With nothing to tell the LAN by, a Public network still gets its line.
$r = Invoke-Scenario 'I no LAN, public network' @{ Lan = @(); Network = 'Public' }
Assert-Says $r "WARN  network 'Shop' (Ethernet) is Public"

# J. The start itself is refused.
$r = Invoke-Scenario 'J start refused' @{ StartCode = 1058; Answers = $false; Pairing = $null }
Assert-Says $r 'FAIL  sc.exe start pos-edge failed with code 1058'

# K. The old process will not stop.
$r = Invoke-Scenario 'K stop hangs' @{ StopHangs = $true }
Assert-Says $r 'WARN  the old pos-edge process did not stop within 60 seconds'

# L. A full URL from the edge (advertised_ip set) is printed as it is.
$r = Invoke-Scenario 'L advertised' @{ Pairing = 'http://10.0.0.4:8787/pair?code=333333' }
Assert-Says $r 'http://10.0.0.4:8787/pair?code=333333'
Assert-Silent $r '192.168.1.20:8787/pair'

# P. ADR-0174: one path, the split layout, and the service's own account, with LocalSystem kept
#    whenever a step fails. What only a real Windows can show is in gate-register row P12.
function Get-Starts($result) { @($result.Events | Where-Object { $_ -eq 'start' }).Count }
function Get-Account($result) { @($result.Events | Where-Object { $_ -like 'account *' }) | Select-Object -Last 1 }

$r = Invoke-Scenario 'P an old box moves' @{}
Assert-Says $r 'ok    pos-edge runs as NT SERVICE\pos-edge, and only it and administrators can read'
Assert-True $r ((Test-Path (Join-Path $r.Root 'data/store.sqlite')) -and (Test-Path (Join-Path $r.Root 'data/store.sqlite-wal'))) 'moves the database with its WAL into data\'
Assert-True $r (-not (Test-Path (Join-Path $r.Root 'store.sqlite'))) 'leaves no database in the root'
Assert-True $r ((Get-Content -Raw (Join-Path $r.Root 'config.toml')) -match 'store_path = ".*data.store\.sqlite"') 'points store_path into data\'
Assert-True $r ((Get-Starts $r) -eq 2) 'starts once as LocalSystem before the account changes'
$first = [array]::IndexOf($r.Events, 'start')
$switch = [array]::IndexOf($r.Events, 'account NT SERVICE\pos-edge')
Assert-True $r ($first -ge 0 -and $switch -gt $first -and [array]::LastIndexOf($r.Events, 'start') -gt $switch) 'changes the account between the two starts'
Assert-True $r ($r.Events -contains 'privs SeChangeNotifyPrivilege/SeCreateSymbolicLinkPrivilege') 'restricts the service to its two privileges'
Assert-True $r (@($r.Events | Where-Object { $_ -like 'secedit *SeCreateSymbolicLinkPrivilege = `*S-1-5-32-544,`*S-1-5-80-1-2-3-4-5' }).Count -eq 1) 'grants the symbolic-link right beside whoever holds it'
Assert-True $r (@($r.Icacls | Where-Object { $_ -like '*data /inheritance:r /grant:r*' }).Count -eq 1) 'closes data\ with inheritance off'

$r = Invoke-Scenario 'P a first install' @{ Kept = $false; Stale = $false; Registered = $false; Account = $null }
Assert-True $r ((Get-Starts $r) -eq 1) 'has no secrets to move, so starts once'
Assert-True $r ((Get-Account $r) -eq 'account NT SERVICE\pos-edge') 'runs as its own account'
$r = Invoke-Scenario 'P already on its own account' @{ Account = 'NT SERVICE\pos-edge'; OldLayout = $false }
Assert-True $r ((Get-Starts $r) -eq 1) 'does not start as LocalSystem again'
Assert-Says $r 'ok    pos-edge runs as NT SERVICE\pos-edge'

$r = Invoke-Scenario 'P another root' @{ RootArg = 'C:\Users\Public\pos-edge' }
Assert-Says $r 'THE INSTALLER STOPPED: refusing -Root C:\Users\Public\pos-edge'
Assert-True $r (-not (Test-Path (Join-Path $r.Root 'config.toml')) -and -not (Test-Path (Join-Path $r.Root 'data'))) 'changes nothing'
$r = Invoke-Scenario 'P data planted by a user' @{ ForeignOwner = 'data'; Stale = $true; OldLayout = $false }
Assert-Says $r 'refusing'
Assert-Says $r 'owned by S-1-5-21-1-2-3-1001'
Assert-True $r (-not (Test-Path (Join-Path $r.Root 'config.toml'))) 'changes nothing'
$link = Join-Path $work 'P-link-target'
New-Item -ItemType Directory -Path $link -Force | Out-Null
$env:ProgramData = Join-Path $work 'P-data-is-a-link'
New-Item -ItemType Directory -Path (Join-Path $env:ProgramData 'pos-edge') -Force | Out-Null
New-Item -ItemType SymbolicLink -Path (Join-Path $env:ProgramData 'pos-edge/data') -Target $link | Out-Null
$r = Invoke-Scenario 'P data is a link' @{ Stale = $false; OldLayout = $false }
Assert-Says $r 'is a link or junction to another place'

$r = Invoke-Scenario 'P the ACL reads back wrong' @{ AclDrift = $true }
Assert-Says $r 'FAIL  the permissions did not come out as ADR-0174 sets them'
Assert-Says $r 'grants access to S-1-5-11'
Assert-Says $r 'The service stays on LocalSystem'
Assert-True $r ((Get-Account $r) -eq 'account LocalSystem') 'stays on LocalSystem'
Assert-Says $r 'ok    pos-edge 0.14.0 is answering on port 8787'
$r = Invoke-Scenario 'P icacls refuses' @{ IcaclsFails = $true }
Assert-Says $r 'failed with exit code 5: Access is denied.'
Assert-True $r ((Get-Account $r) -eq 'account LocalSystem') 'stays on LocalSystem'
$r = Invoke-Scenario 'P sc.exe privs refuses' @{ ScFails = 'privs' }
Assert-Says $r 'sc.exe privs pos-edge failed with exit code 5'
Assert-True $r ((Get-Account $r) -eq 'account LocalSystem') 'puts the account back'
$r = Invoke-Scenario 'P an old release makes no vault key' @{ MakesVaultKey = $false }
Assert-Says $r 'FAIL  the store made no vault key'
Assert-True $r ($r.Icacls.Count -eq 0 -and $null -eq (Get-Account $r)) 'changes no permission and no account'
$r = Invoke-Scenario 'P a secret stays in Credential Manager' @{ FirstLog = 'WARN pos_edge::vault: could not move a secret from the OS keyring into the sealed vault' }
Assert-Says $r 'FAIL  the store could not move every secret out of Credential Manager'
Assert-True $r ($null -eq (Get-Account $r)) 'keeps the account'
$r = Invoke-Scenario 'P the first start never answers' @{ Answers = $false; Pairing = $null }
Assert-Says $r 'FAIL  the store did not answer on its first start as LocalSystem'
Assert-True $r ($null -eq (Get-Account $r)) 'keeps the account'

Remove-Item -LiteralPath $work -Recurse -Force
Write-Host ''
if ($script:failures -gt 0) {
    Write-Host "installer behaviour: $($script:failures) check(s) failed"
    exit 1
}
Write-Host 'installer behaviour: all checks passed'
