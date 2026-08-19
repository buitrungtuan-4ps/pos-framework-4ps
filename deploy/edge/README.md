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

## Windows — a service

Windows is a supported store OS. Until the native service wrapper (the `windows-service` crate) lands
with hardware bring-up (roadmap A5) — it is platform code that cannot be exercised on the Linux CI —
install the binary as a service with the built-in Service Control Manager, which delivers the same
graceful-stop signal the binary already handles:

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
(`bind`, `store_id`, and optionally `advertised_ip`). Everything else is the cloud-owned configuration
the edge syncs and hot-reloads at runtime ([ADR-0004](../../docs/adr/0004-cloud-owned-configuration.md)).
