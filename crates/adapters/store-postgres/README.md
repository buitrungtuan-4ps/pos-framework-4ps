# `store-postgres`

**Layer** `adapter` · **Owners** @maintainers-cloud

## What it is

The cloud's `EventStore` over PostgreSQL, and the tables behind every other cloud store (configuration, people, devices, catalogue, rollups, API keys …). Hand-written SQL over `tokio-postgres` and `deadpool`. The event log is partitioned monthly and tenant-isolated by row-level security.

## Owns

- `EventStore` (`PostgresStore`)
- One store type per cloud area, `Postgres…`
- The cloud's migrations

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-cloud`, `pos-cloud-api`, `pos-cloud-config`, `pos-cloud-devices`, `pos-cloud-inventory`, `pos-cloud-reports`.

## Entry points

- `src/store.rs`: `PostgresStore`
- `src/<area>.rs`: one cloud store per file
- `migrations/`: numbered SQL migrations, additive only

## Rules

- [ADR-0008](../../../docs/adr/0008-postgres-partitioning.md) · [ADR-0016](../../../docs/adr/0016-postgres-access.md) · [ADR-0017](../../../docs/adr/0017-migrations.md) · [ADR-0022](../../../docs/adr/0022-events-partition-strategy.md)

## How to test

```bash
cargo test -p store-postgres
DATABASE_URL=… cargo test -p store-postgres --features integration -- --test-threads=1
```

`tests/integration.rs` runs every store against one shared database, which its cases truncate between
them. `tests/migrate.rs` holds `PostgresStore::migrate` to the files in `migrations/`: each of its
cases creates a database of its own and drops it afterwards, so the role in `DATABASE_URL` must be
allowed to create databases.
