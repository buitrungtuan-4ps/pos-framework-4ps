# `pos-cloud-floor`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's floor and kitchen: a store's areas and tables, its kitchen stations and the rules that route each item to one, the compile of those to the store's `floor` and `stations` nodes and their publish, and the signed table codes a guest's QR order carries. It stands on `pos-cloud-api` (authentication, the audit trail and the refusals), `pos-cloud-config` (the publish every area writes through, and the registry) and `pos-cloud-catalog` (a routing rule names a menu item). `pos-cloud` re-exports `floorplan`, `floor_compiler` and `qr` under their old paths, imports `routes` into its `http` module as `floor`, and keeps the guest-facing `POST /v1/qr/orders` endpoint, which reads its application state.

## Owns

- Areas, tables, stations and routing rules, and the `AreaStore`, `TableStore`, `StationStore` and `RoutingRuleStore` traits
- The compile of a store's floor to its `floor` and `stations` nodes, with the referential checks before a publish
- The signed table token and the QR ordering guardrail decision
- Their routes: `/admin/floor/*` (areas, tables, the publish and the table QR codes) and `/admin/kitchen/*` (stations and routing)
- Its stores over PostgreSQL, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-cloud-catalog`, `pos-cloud-config`, `pos-core`, `pos-proto`, `store-postgres`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/floorplan.rs`: the records and the four store traits
- `src/floor_compiler.rs`: `compile_floor`, `compile_stations`
- `src/qr.rs`: `TableTokenSecret`, `mint_table_token`, `verify_table_token`, `evaluate`
- `src/routes.rs`: `floor_router`; `src/routes/`: `floor_publish_router`, `table_qr_router`, `floor_nodes`
- `src/persistence.rs`: the store-postgres impls

## Rules

- [ADR-0057](../../docs/adr/0057-qr-ordering.md): QR ordering
- [ADR-0072](../../docs/adr/0072-floor-and-kitchen.md): the floor and kitchen
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4 and [ADR-0175](../../docs/adr/0175-the-crate-list-follows-what-a-move-can-reach.md): why this is a crate

## How to test

```bash
cargo test -p pos-cloud-floor
```

The routes' HTTP tests drive the whole router, so they stay in `crates/pos-cloud/tests/`.
