# `pos-edge`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

The store binary, `pos_edge`, and its library. It owns the async runtime, the HTTP and WebSocket surface, and the transaction. It composes the synchronous `pos-core` domain with the adapters the fork selected, serves the embedded till UI (`ui/dist`) to every device on the store network, and fans each committed change out to them.

## Owns

- The `/api/*` routes the till calls, and their permission table
- The application layer: load, decide, apply, publish (`Edge`)
- Device pairing, sign-in sessions and the idle window
- Printing, the print queue, OTA, backups, and the cloud clients

## Depends on / used by

- **Depends on** (workspace crates): `cloud-sync-http`, `key-vault-keyring`, `link-nats`, `pos-core`, `pos-country`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-ports`, `pos-proto`, `pos-render`, `printer-escpos`, `store-sqlite`, `updater-minisign`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `minimal-edge`.

## Entry points

- `src/main.rs`: the binary; deliberately tiny
- `src/server.rs`: `serve` and `compose`: wires the adapters and binds the listener
- `src/app.rs`: `Edge`, every command
- `src/http/mod.rs`: `router`, every route; one file per area beside it
- `src/http/route_permissions.rs`: what each state-changing route needs
- `src/auth.rs`: `Sessions`, sign-in and idle

## Rules

- [ADR-0002](../../docs/adr/0002-one-binary-per-tier.md) · [ADR-0013](../../docs/adr/0013-async-strategy.md) · [ADR-0018](../../docs/adr/0018-http-websocket-stack.md) · [ADR-0084](../../docs/adr/0084-device-authentication.md) · [ADR-0091](../../docs/adr/0091-durable-edge-auth-state.md) · [ADR-0111](../../docs/adr/0111-a-second-origin-may-address-the-edge.md) · [ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)

## How to test

```bash
cargo test -p pos-edge
POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-edge --test routes_snapshot    # after adding a route
just run-edge    # the store on in-memory fakes (examples/minimal-edge)
```
