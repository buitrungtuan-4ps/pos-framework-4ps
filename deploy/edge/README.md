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

Run the four commands as an administrator:

```
sc.exe create pos-edge binPath= "C:\pos\pos-edge.exe" start= auto
sc.exe description pos-edge "Pizza 4P's POS edge (store server)"
setx POS_EDGE_CONFIG "C:\pos\config.toml" /M
sc.exe failure pos-edge reset= 86400 actions= restart/5000/restart/5000/restart/30000
sc.exe start pos-edge
```

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
  a rename or a signature change fails a pull request. That a real service reaches `RUNNING`, that a
  stop drains, and that a failure action restarts on exit code 1 are checks that need a Windows box
  with a service installed on it — a row in [`docs/gate-register.md`](../../docs/gate-register.md),
  not a claim made here.

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
