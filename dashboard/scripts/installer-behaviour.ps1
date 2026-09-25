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
Set-Content -LiteralPath $binary -Value 'not a real binary'

$store = '01M2MQ2BH6PKH6W4SEN2YVP9VT'
$script:failures = 0

# ---------------------------------------------------------------------------------------------
# The fake Windows. Every function reads $global:scene, which each scenario sets.
# ---------------------------------------------------------------------------------------------

function global:sc.exe {
    $verb = $args[0]
    switch ($verb) {
        'start' {
            if ($global:scene.StartCode -ne 0) {
                $global:LASTEXITCODE = $global:scene.StartCode
                return "[SC] StartService FAILED $($global:scene.StartCode):"
            }
            # SCM answers at once; the edge itself takes a moment, and writes its pairing file and binds
            # the port only on the installer's first wait (see Start-Sleep). An edge that wrote it here
            # would hide the race a real re-install hits.
            $global:scene.Started = $true
            $global:LASTEXITCODE = 0
            return '[SC] StartService SUCCESS'
        }
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
    if (-not $global:scene.Listening -or -not $global:scene.Answers) { throw 'connection refused' }
    $global:scene.Events.Add('health')
    return [pscustomobject]@{ status = 'ok'; version = $global:scene.Running; store_id = $global:scene.AnsweringStore }
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
    $root = Join-Path $work ($Name -replace '[^A-Za-z0-9]', '-')
    New-Item -ItemType Directory -Path $root -Force | Out-Null
    $global:scene = @{
        Registered = $true; Started = $false; Listening = $false; StartCode = 0; StopHangs = $false
        Pairing = '/pair?code=222222'; PairingPath = (Join-Path $root 'pairing-url.txt')
        Answers = $true; AnsweringStore = $store; PortHolder = $null
        Network = 'Private'; Vpn = $null; Lan = @('192.168.1.20'); Cloud = 'ok'; ClockSkewMinutes = 0
        Running = '0.14.0'; Carried = ''
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
    if ($global:scene.Stale) { Set-Content -LiteralPath $global:scene.PairingPath -Value '/pair?code=111111' }
    if ($null -ne $global:scene.Log) { Set-Content -LiteralPath (Join-Path $root 'pos-edge.log') -Value $global:scene.Log }

    # A script that stops half-way is the worst outcome for a technician, so a scenario that ends in
    # an exception is kept as text for the assertions to fail on, not left to end this run.
    try {
        $output = & $installer -Binary $binary -StoreId $store -CloudUrl 'https://cloud.example.com' -Root $root -OpenSetup -CarriedVersion $global:scene.Carried 6>&1 3>&1 2>&1 |
            ForEach-Object { "$_" }
    } catch {
        $output = @("THE INSTALLER STOPPED: $($_.Exception.Message)")
    }
    return [pscustomobject]@{ Name = $Name; Text = ($output -join "`n"); Events = @($global:scene.Events) }
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
$r = Invoke-Scenario 'M older release running' @{ Running = '0.11.0'; Carried = '0.14.0' }
Assert-Says $r 'runs 0.11.0, which was kept; the 0.14.0 this installer carries'
Assert-Says $r "To run 0.14.0 here, roll it out from the console's OTA screen."
$r = Invoke-Scenario 'M newer release running' @{ Running = '0.14.0'; Carried = '0.13.0' }
Assert-Says $r 'runs 0.14.0, newer than the 0.13.0 this installer carries, so it was kept.'
Assert-Silent $r 'To run 0.13.0'
$r = Invoke-Scenario 'M same release' @{ Running = '0.14.0'; Carried = '0.14.0' }
Assert-Says $r 'the binary it runs was kept (version 0.14.0)'

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

Remove-Item -LiteralPath $work -Recurse -Force
Write-Host ''
if ($script:failures -gt 0) {
    Write-Host "installer behaviour: $($script:failures) check(s) failed"
    exit 1
}
Write-Host 'installer behaviour: all checks passed'
