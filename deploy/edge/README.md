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

Windows is a supported store OS. Until the native service wrapper (the `windows-service` crate) lands
with hardware bring-up (roadmap A5) — it is platform code that cannot be exercised on the Linux CI —
install the binary as a service with the built-in Service Control Manager, which delivers the same
graceful-stop signal the binary already handles. Over-the-air updates stay off on Windows until that
slice lands: the install seam needs a symlink a service account can create, and the edge detects the
absence rather than failing an install every ten minutes.

```
sc.exe create pos-edge binPath= "C:\pos\pos-edge.exe" start= auto
sc.exe description pos-edge "Pizza 4P's POS edge (store server)"
setx POS_EDGE_CONFIG "C:\pos\config.toml" /M
sc.exe start pos-edge
```

A wrapper such as NSSM works equally well and adds log rotation. Configure the service to send
`CTRL+BREAK`/stop so the edge drains rather than being killed outright.

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
URL is the field that would carry a credential (`nats://user:pass@host`), which is why it is not in
`config.toml`. With either missing the edge logs it and publishes nothing: the store trades and its
outbox holds every event until a stream exists, which is also what happens while the cloud is down.

**The store key needs two scopes**, `read_config` **and** `relay_orders`
([ADR-0087](../../docs/adr/0087-edge-relay-and-event-publish.md)): the first for config-pull and the
heartbeat, the second for the order relay, which pulls the store's cloud-placed orders and acks each
outcome. A key issued with `read_config` alone leaves the relay dark — the symptom is a repeated
`the cloud refused the order pull with status 403` in the edge log, every five seconds. Nothing else
is affected: the counter trades, config syncs, and the orders stay parked in the cloud until the
scope is granted.
