# `pos-cloud-inventory`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's inventory: the ingredients, recipes and supplier references a tenant authors under `/admin/inventory/*`, and their publish to each store as its `inventory` node through `/admin/config/inventory`. It stands on `pos-cloud-api` (the console's permission check, audit entry and refusals) and `pos-cloud-config` (the publish every area writes through). `pos-cloud` re-exports `inventory` under its old path, mounts the two routers, and uses `inventory_nodes` in its store-group batch.

## Owns

- Ingredients, recipes and supplier references, and the `InventoryStore` trait
- The compile of a tenant's inventory to the `inventory` node
- The `/admin/inventory/*` routes and the `/admin/config/inventory` publish
- Its store over PostgreSQL: the `inventory_items` table, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-cloud-config`, `pos-proto`, `store-postgres`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/inventory.rs`: `InventoryStore`, `to_node`
- `src/routes.rs`: `inventory_router`, the request bodies and their builders
- `src/routes/publish.rs`: `config_inventory_router`, `inventory_nodes`
- `src/persistence.rs`: the store-postgres impl

## Rules

- [ADR-0079](../../docs/adr/0079-inventory-and-suppliers.md): inventory and suppliers
- [ADR-0095](../../docs/adr/0095-conditional-writes-for-collections.md): conditional writes
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4: why this is a crate

## How to test

```bash
cargo test -p pos-cloud-inventory
```

The routes' HTTP tests drive the whole router, so they stay in `crates/pos-cloud/tests/cloud/inventory.rs`.
