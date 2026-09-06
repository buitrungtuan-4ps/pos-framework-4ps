# Running `pos_edge` as a service

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-19

The store binary runs unattended on a mini-PC ([ADR-0003](../../docs/adr/0003-cattle-not-pets.md)), so
it is installed as an operating-system service that starts on boot and restarts on crash. The binary
itself is service-agnostic: it reads `POS_EDGE_CONFIG`, logs through `tracing`, and shuts down
gracefully on Ctrl-C or `SIGTERM` (`crates/pos-edge/src/server.rs`), which is all any service manager
needs.

## Linux — systemd

Use [`pos-edge.service`](pos-edge.service). The install steps are in its header comment. On stop,
systemd sends `SIGTERM`; the edge drains in-flight requests before exiting, so a committed sale is
durable and an interrupted one was never acknowledged.

### The binary lives under the state directory

`ExecStart` is **`/var/lib/pos-edge/bin/current`**, a symlink, not `/usr/local/bin/pos-edge`
([ADR-0055](../../docs/adr/0055-edge-ota-updater.md) Amendment 1). The unit runs the store as an
unprivileged user under `ProtectSystem=strict` and `NoNewPrivileges`, which make everything outside
its `StateDirectory` read-only to the process — so an over-the-air update cannot write
`/usr/local/bin`, and giving it that privilege would let a compromised till replace system binaries.
The layout:

```
/var/lib/pos-edge/bin/current      -> slot-a | slot-b   what systemd starts
/var/lib/pos-edge/bin/previous     -> slot-a | slot-b   where a rollback goes back to
/var/lib/pos-edge/bin/slot-a       a version, mode 0755
/var/lib/pos-edge/bin/slot-b       the other one
/var/lib/pos-edge/bin/unconfirmed  present while a new version has not booted healthy yet
/var/lib/pos-edge/store.sqlite.pre-update   the database as it was before the install
```

An install writes the spare slot, retargets `current` with one atomic `rename(2)`, and **exits**;
`Restart=always` is what turns that exit into a start on the new binary. If a committed version
never reaches a healthy boot, the edge counts its attempts, and past three it points `current` back
at `previous`, restores the `.pre-update` database and exits again — so a bad release heals itself
instead of needing somebody at the shop.

`/usr/local/bin/pos-edge` is still worth keeping: it is the operator's rescue copy and what
`pos-edge --self-test` is run from by hand. It is simply not what the service runs.

**A box laid out the old way keeps trading.** With no `bin/current` the edge logs that it found no
layout and starts no updater; everything else — pairing, selling, config-pull, heartbeat, the relay
— is unchanged. To migrate an existing store, follow the unit's install block (create `bin/`, copy
the running binary to `slot-a`, link `current` at it) and change `ExecStart`.

## Windows — a service

Windows is a supported store OS, and the binary speaks the Service Control Manager's protocol itself
(roadmap **E4**, `crates/pos-edge/src/service.rs`): started by SCM it reports `RUNNING`, a stop
drains in-flight requests exactly as a `SIGTERM` does on Linux, and a machine shutdown is treated as
a stop. Started from a console it notices that SCM is not there and runs in the foreground, so
`pos-edge.exe --self-test` and an operator's manual run are unchanged. **No third-party wrapper is
needed.**

### Use the installer

`install-pos-edge.ps1`, beside this file, does the whole install. Run it in an elevated PowerShell:

```
powershell -ExecutionPolicy Bypass -File .\install-pos-edge.ps1 `
    -Binary .\pos-edge.exe -StoreId 01J... -CloudUrl https://cloud.example.com -SyncKey <the store's key>
```

The new-store wizard's Handoff screen hands out the same script with the store's values already in
it, so a technician runs one command with no arguments to get wrong. Both are emitted by
`dashboard/src/installers.mjs` and parsed by the `build (windows-2022)` CI job before they can ship.
Running either twice is safe.

Prefer it over the by-hand recipe below, which is here to say what the script does and to be
debuggable when something is already half-installed. **Three of its lines are easy to get wrong, and
each has a silent failure mode**, which is the reason the script exists:

```
sc.exe create pos-edge binPath= "C:\ProgramData\pos-edge\bin\current" start= auto
sc.exe description pos-edge "Pizza 4P's POS edge (store server)"
sc.exe failure pos-edge reset= 86400 actions= restart/5000/restart/5000/restart/30000
sc.exe start pos-edge
```

* **`binPath` is `bin\current`, not the binary.** `current` is the symlink the edge retargets when it
  installs an update (ADR-0055 Amendment 1). A service registered straight onto `pos-edge.exe`
  trades perfectly well and silently never self-updates — the same mistake the Linux installer
  exists to prevent.
* **`config.toml` needs an absolute `store_path`.** SCM starts a service in `C:\Windows\System32`,
  and a relative path is resolved against the working directory, so the default `store.sqlite` puts
  the store's database — and `bin\`, which is derived from the database's parent — under `System32`.
  The systemd unit sets `WorkingDirectory=` and has no equivalent problem. Write
  `store_path = "C:\\ProgramData\\pos-edge\\store.sqlite"` (TOML, so the separators are doubled).
* **The `failure` line is not optional.** See below.

`POS_EDGE_CONFIG` and the store key go in the service's own registry key, not in a machine-wide
variable — the next section says why.

### The two secrets Windows has no `env` file for

The Linux unit reads `POS_EDGE_SYNC_KEY` and `POS_EDGE_NATS_URL` from `/etc/pos-edge/env`,
root-owned mode 0600. Windows has no such file and no `EnvironmentFile=`, and until now nothing here
said what to do instead — so a Windows store could be configured right and still never sync or
publish (production-readiness **D4**).

**Prefer the credential store.** The store's sync key belongs in Credential Manager, which is where
activation puts the device credential and where the edge looks first
([ADR-0086](../../docs/adr/0086-edge-keyvault-and-activation.md)). `POS_EDGE_SYNC_KEY` is a
**headless bring-up override**, exactly as on Linux.

When you do need the override, or the event-bus URL (which has no keyring slot), set them as
**service-scoped** values rather than machine-wide ones — a machine environment variable is readable
by every local administrator and shows up in process listings of unrelated services:

```
reg add "HKLM\SYSTEM\CurrentControlSet\Services\pos-edge" /v Environment /t REG_MULTI_SZ ^
  /d "POS_EDGE_CONFIG=C:\pos\config.toml\0POS_EDGE_SYNC_KEY=<the store's scoped key>\0POS_EDGE_NATS_URL=tls://:<token>@<your cloud host>:4222" /f
sc.exe stop pos-edge & sc.exe start pos-edge
```

`REG_MULTI_SZ` with `\0` separators is how SCM passes several variables to one service; the values
are then visible only to accounts that can read that service key, not to every process on the box.
Restart the service after changing them — SCM reads the value at start, not on the fly. This is
where `POS_EDGE_CONFIG` belongs too, and it is what the installer writes; a machine-wide
`setx POS_EDGE_CONFIG … /M` also works but is readable by every local administrator, and having both
leaves the next person guessing which one the service actually read.

### The `failure` line is not optional

It is the Windows counterpart of the unit's `Restart=always`, and without it the store does not come
back from an update or a crash. SCM has no "always restart" setting — it has **failure actions**, and
it applies them only when a service looks like it failed. So:

- An **install** retargets `bin\current` and exits with code **1**, which SCM reads as a failure and
  the action above restarts five seconds later, on the new binary. Exiting `0` would look like a
  deliberate stop and the shop would stay dark until somebody drove there.
- An **operator's `sc.exe stop`** exits `0` and the service stays stopped, which is what was asked
  for.
- A **failed start** — unreadable config, port already bound, a store database that will not open —
  also exits `1`, so the same action retries it rather than leaving a dead service.

`reset= 86400` means the failure count clears after a day, so three bad days in a row do not exhaust
the action list.

### Over-the-air updates on Windows

The slot layout is the same as on Linux and the same code lays it out; only the two primitives differ
(`CreateSymbolicLinkW` instead of `symlink(2)`, and no permission bit to set). Two things are worth
knowing:

- **Creating a symlink needs a privilege.** `SeCreateSymbolicLinkPrivilege` is held by
  Administrators and by `LocalSystem`, which is the account `sc.exe create` uses when no `obj=` is
  given. A service running as a named low-privilege account will not have it unless it was granted,
  and the edge reports the absence rather than failing an install every ten minutes: with no
  `bin\current` the updater is not started at all and the box trades on the binary it has.
- **Nothing in this repository has watched it happen.** The Windows CI job compiles the wrapper, so
  a rename or a signature change fails a pull request, and it now also *parses* `install-pos-edge.ps1`
  and the wizard's generated one, so a script that could not run at all cannot ship. Neither is the
  same as running them. That a real service reaches `RUNNING`, that a stop drains, and that a failure
  action restarts on exit code 1 are checks that need a Windows box with a service installed on it —
  a row in [`docs/gate-register.md`](../../docs/gate-register.md), not a claim made here.

A wrapper such as NSSM still works if you want its log rotation, but it is no longer what makes the
service run.

## Configuration

Both platforms point `POS_EDGE_CONFIG` at a `config.toml` holding the bootstrap configuration
(`bind`, `store_id`, optionally `advertised_ip`, and — for a store connected to a cloud — `cloud_url`).
Everything else is the cloud-owned configuration the edge syncs and hot-reloads at runtime
([ADR-0004](../../docs/adr/0004-cloud-owned-configuration.md)).

With `cloud_url` set the edge serves the activation routes (`POST /api/activate`), keeps the device
credential in the OS credential store (Credential Manager on Windows, the kernel keyring on Linux —
[ADR-0086](../../docs/adr/0086-edge-keyvault-and-activation.md)), and — once activated — runs the
config-pull, heartbeat, and order-relay loops. Those loops authenticate with the store's scoped key,
read from the keyring (`sync_key`) or, as a headless bring-up override, from `POS_EDGE_SYNC_KEY`
(the unit's optional `/etc/pos-edge/env`, root-owned mode 0600 — never in `config.toml`, never
committed). Without `cloud_url` the edge runs LAN-only, exactly as before.

**Publishing the store's events** takes one more pair of settings
([ADR-0087](../../docs/adr/0087-edge-relay-and-event-publish.md)): a `[nats]` section in
`config.toml` naming the `stream` and `subject` — which must match the cloud consumer's `stream` and
`filter_subject` — and the server URL in `POS_EDGE_NATS_URL`, from the same mode-0600 env file. The
URL is the field that would carry a credential, which is why it is not in `config.toml`. With either
missing the edge logs it and publishes nothing: the store trades and its outbox holds every event
until a stream exists, which is also what happens while the cloud is down.

The console's new-store wizard generates the `[nats]` section, so on a provisioned box the section is
already right and only the URL is left to fill in. Both of its values are the **fleet's**, identical
on every store — `stream = "POS_FLEET"`, `subject = "pos.fleet.events"` ([ADR-0087](../../docs/adr/0087-edge-relay-and-event-publish.md)
Amendment 1). Per-store streams look tidier and do not work: `pos_cloud` binds one durable consumer
to one named stream, so it would ingest one store and ignore the rest.

The URL's shape is `tls://:<token>@<your cloud host>:4222`. The `tls://` scheme is what makes the
client require TLS — `nats://` connects in plaintext and a broker publishing `4222` refuses it — and
the token belongs in the userinfo exactly as shown, recovered on the cloud box with
`sudo sed -n 's/  token: //p' deploy/secrets/nats.conf`. It is one secret for the whole fleet, which
is why the console does not put it in the file for you.

**The store key needs two scopes**, `read_config` **and** `relay_orders`
([ADR-0087](../../docs/adr/0087-edge-relay-and-event-publish.md)): the first for config-pull and the
heartbeat, the second for the order relay, which pulls the store's cloud-placed orders and acks each
outcome. A key issued with `read_config` alone leaves the relay dark — the symptom is a repeated
`the cloud refused the order pull with status 403` in the edge log, every five seconds. Nothing else
is affected: the counter trades, config syncs, and the orders stay parked in the cloud until the
scope is granted.

## Printing: fonts, and the cable

Two deployment facts the store needs and the binary cannot supply.

### Install a font package, or the store prints ASCII only

A thermal printer renders text from a few hundred glyphs in its own firmware, which does not reach
Vietnamese and is nowhere near Japanese or Devanagari. So anything outside plain ASCII is **drawn by
the edge** and sent as an image ([ADR-0102](../../docs/adr/0102-printing-any-script.md)). That needs
fonts, and fonts are a deployment asset rather than framework code — embedding one would ship every
store megabytes it will never print and still not cover the next country.

| Script | Debian/Ubuntu package |
|---|---|
| Latin **and Vietnamese** | `fonts-dejavu-core` |
| Japanese, Chinese, Korean | `fonts-noto-cjk` |
| Devanagari (Hindi, Marathi, Nepali) | `fonts-noto-devanagari` |
| Thai | `fonts-noto-thai` |
| Arabic | `fonts-noto-arabic` |

```sh
sudo apt-get install -y fonts-dejavu-core        # the minimum for a Vietnamese store
```

On Windows the shipped fonts in `C:\Windows\Fonts` already cover Latin and Vietnamese; add a Noto
face to that directory for anything else.

The edge scans the platform's font directories by default and needs no configuration. Point it
somewhere else — a font kept with the application, say — with:

```toml
font_directories = ["/opt/pos-edge/fonts"]
font_size_dots   = 24   # printer dots per em; 24 is a comfortable receipt body at 203 dpi
```

Directories are scanned recursively, in order, and that order is the fallback order: the face for
ordinary Latin text goes first.

**Check it worked.** The edge logs one line at start-up naming what it can print:

```
INFO printing fonts loaded faces=1 can_print=["Latin", "Vietnamese"] cannot_print=["Japanese", ...]
```

If the line says `no printing fonts found`, the store trades and prints only ASCII — every
Vietnamese ticket will be refused. That log line is the whole early warning; without it a missing
package first shows up as a kitchen ticket that never arrives during service.

### A USB or serial printer is a device path

Approve the printer in the console with its **connection** set to USB or Serial, and put the OS
device path in the address field rather than a host
([ADR-0103](../../docs/adr/0103-directly-attached-printers.md)):

| Connection | Address |
|---|---|
| Network | `192.168.1.50:9100` (bare host gets port 9100) |
| USB, Linux | `/dev/usb/lp0` |
| Serial, Linux | `/dev/ttyUSB0`, `/dev/ttyS0` |
| Serial, Windows | `\\.\COM3` |

Two Linux details:

- **The service user needs permission.** USB and serial nodes are owned by group `lp` (or `dialout`
  for some serial adapters). Without it every print is `Unavailable` and the log says the printer
  could not be reached, which reads like an unplugged cable.
  ```sh
  sudo usermod -aG lp pos-edge
  ```
- **A serial printer needs its baud rate set outside the process.** USB printer-class devices have
  no baud rate and need nothing; serial ones do, and the edge does not set it (that would mean
  `termios` FFI for a legacy connection). Add it to the unit:
  ```ini
  ExecStartPre=/bin/stty -F /dev/ttyUSB0 19200 raw -echo
  ```
  Match the printer's own setting — 9600 and 19200 are the common ones, and it is usually printed on
  a self-test page the printer produces when you hold the feed button while powering it on.

**A cash drawer does not open yet.** The USB channel it needs now exists — and a drawer may only ever
open over USB, because port 9100 has no authentication (`docs/architecture.md` §5) — but nothing in
the published device says a drawer is *wired* to a given printer, so none is kicked. ADR-0103 names
the console field that closes it.

## The print agent — for a store whose edge is not in the shop

A printer's last hop is a cable, and a cable is in a building. When the edge runs somewhere other
than that building ([ADR-0110](../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)), a
printer that is plugged into a terminal cannot be dialled from it — so the terminal claims the
rendered job instead and writes the bytes itself
([ADR-0112](../../docs/adr/0112-print-agents.md)).

**A store whose edge is in the shop needs none of this**, and should install none of it. That is not
a recommendation; it is what the design is for. A published printer that names no agent is opened by
the edge exactly as it always was.

### What has to be true before it prints

Four things, in this order, and skipping any one of them leaves an agent that runs and claims
nothing:

1. **A `TERMINAL` entry exists in the console** for this machine. Nothing discovers one — a terminal
   is not a printer announcing itself on the LAN — so an admin creates it, under **Devices →
   Terminals**, with the store chosen in the top bar.
2. **The printer's device entry names that terminal** as its agent, and the config is published.
   Under **Devices → Print agents**: pick the terminal beside the printer, then publish.
3. **This machine is paired with the store's edge**, which is what mints the device token below.
4. **A manager binds this device to the terminal entry**, at the till, signed in. The binding is
   exclusive: one terminal, one machine, refused rather than promoted, because two machines holding
   one identity split a kitchen's tickets between them and nobody notices until service.

Until (4) the agent runs and is told, on every claim, that it answers for no print agent. That is the
honest state rather than a fault, and the log line says exactly that.

### Linux

```sh
sudo install -d -o pos-agent -g pos-agent /var/lib/pos-print-agent
sudo install -m 0755 pos_print_agent /usr/local/bin/pos_print_agent
sudo tee /var/lib/pos-print-agent/print-agent.toml >/dev/null <<'TOML'
edge_url   = "https://store-01.example.com"     # or http://192.0.2.10:8787 on a shop LAN
state_path = "/var/lib/pos-print-agent/state.json"
TOML
sudo install -d -m 0750 /etc/pos-print-agent
printf 'POS_PRINT_AGENT_TOKEN=%s\n' "$TOKEN" | sudo tee /etc/pos-print-agent/env >/dev/null
sudo chmod 0600 /etc/pos-print-agent/env
sudo cp pos-print-agent.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl enable --now pos-print-agent
```

The token is **not** in the config file, deliberately: it is a credential, and the config sits beside
the state where whoever is diagnosing a printer will read it. Same split the edge makes for
`POS_EDGE_SYNC_KEY`.

The unit puts the agent in the `lp` group and allows the three character-device classes a printer
uses. A network printer needs none of that; a `/dev/usb/lp0` one needs all of it.

### Windows

```powershell
powershell -ExecutionPolicy Bypass -File .\install-pos-print-agent.ps1 `
    -Binary .\pos_print_agent.exe -EdgeUrl https://store-01.example.com -DeviceToken <token>
```

`install-pos-print-agent.ps1` is generated from `dashboard/src/installers.mjs` and checked for drift
and for parse errors by CI, exactly as the edge's is. It sets the service's failure actions, which is
the half that is easiest to skip and the half that decides whether the terminal comes back after a
crash — and a crashed agent means a kitchen printing nothing while the till keeps reporting
`QUEUED_TO_AGENT`, because the queue is doing its job.

### What it does not do

**No over-the-air updates.** The agent holds nothing that matters — one id per printer, so a
reinstall costs at most one duplicated ticket — so it has no update slots and no rollback. Upgrading
is replacing the binary and restarting. The edge's layout exists because a store's data does not
survive being reinstalled; this one's does not need to.
