# `store-sqlite`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The store's one SQLite database, implementing `EventStore`, `ConfigStore`, `DeviceRegistry`, `SubjectStore` and `IntakeLedger` with one transaction type. A dedicated writer thread owns the connection, so blocking calls never touch the async executor and the outbox position is gapless.

## Owns

- The edge's five stores (`SqliteStore`, `SqliteTx`)
- Snapshots and integrity checks
- The edge's migrations

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-edge`, `pos-edge-app`.

## Entry points

- `src/store.rs`: `SqliteStore`
- `src/tx.rs`: `SqliteTx`
- `src/writer.rs`: the writer thread
- `src/migrations.rs`: embeds `migrations/`

## Rules

- [ADR-0015](../../../docs/adr/0015-sqlite-access.md) · [ADR-0017](../../../docs/adr/0017-migrations.md) · [ADR-0026](../../../docs/adr/0026-port-shapes.md) · [ADR-0131](../../../docs/adr/0131-a-chained-event-log.md)

## How to test

```bash
cargo test -p store-sqlite
```
