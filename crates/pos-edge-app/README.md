# `pos-edge-app`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

The store's application layer: `Edge` and every command it loads, decides, applies inside one transaction and publishes, with the projection it folds events into. Beside it are what those commands keep: sign-in sessions, the lockout and the idle window, and their durable registry; the store-LAN fan-out; the clock, the id generator and the clock's drift check; the store lease, queue numbers and receipt numbers, with their SQLite impls; line notes; and what the till shows of the cloud link. The billing, shift and floor commands are `impl Edge` blocks, so they live here with `Edge`. `pos-edge` re-exports every module under its old path (`pos_edge::app`, `pos_edge::auth`, …) and keeps the HTTP routes, the cloud clients, printing, OTA and the server.

## Owns

- `Edge`, its commands by area (`app/orders/`, `app/bills/`, `app/shifts/`, `app/kitchen/`, `app/floor.rs`) and the `Projection`
- Sign-in, the lockout and the idle window (`Sessions`), and the durable device and sign-in registry
- The store-LAN fan-out (`Fanout`)
- The system clock, the id generator, and the SNTP drift check
- The lease, queue-number and receipt-number authorities, and their impls for the SQLite store
- Line notes and the sync status

## Depends on / used by

- **Depends on** (workspace crates): `pos-core`, `pos-ports`, `pos-proto`, `store-sqlite`; in tests, `pos-fakes`.
- **Used by** (workspace crates): `pos-edge`, `pos-edge-printing`.

## Entry points

- `src/app/mod.rs`: `Edge` and the `Projection`; each area's commands beside it under `src/app/`, with the event plumbing every command writes through (`publish.rs`)
- `src/auth.rs`: `Sessions`, sign-in and idle; `src/durable_auth.rs`: `DurableAuth`
- `src/fanout.rs`: `Fanout`, `ServerMessage`
- `src/lease_state.rs`, `src/queue.rs`, `src/receipt.rs`: the authorities and their SQLite impls
- `src/sntp.rs`, `src/sync_status.rs`: the clock and cloud-link status the till shows

## Rules

- [ADR-0013](../../docs/adr/0013-async-strategy.md): the application layer each binary owns
- [ADR-0018](../../docs/adr/0018-http-websocket-stack.md): the fan-out
- [ADR-0030](../../docs/adr/0030-pairing-and-offline-auth.md): offline sign-in
- [ADR-0091](../../docs/adr/0091-durable-edge-auth-state.md): durable sign-in state
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4: why this is a crate
- Each module that logs keeps its old `pos_edge::…` `tracing` target through a `LOG_TARGET` constant, so log filters and the durable log's exclusions ([ADR-0117](../../docs/adr/0117-a-headless-store-keeps-a-log.md) decision 6) are unaffected. A module moved in here keeps its target the same way.

## How to test

```bash
cargo test -p pos-edge-app
```

The HTTP and end-to-end tests drive the whole router, so they stay in `crates/pos-edge/tests/`.
