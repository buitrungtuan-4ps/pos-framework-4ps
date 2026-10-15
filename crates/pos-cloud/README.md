# `pos-cloud`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud binary, `pos_cloud`, and its library. It ingests every store's events idempotently into PostgreSQL, answers dashboards and reports from materialised rollups, composes and publishes each store's configuration, and serves the integrator API (`/v1`) and the console API (`/admin`). Stores reach it outbound on `/sync/*` to pull configuration, report and fetch artifacts. It serves the embedded console (`dashboard/dist`).

## Owns

- Event ingest (a store's HTTPS `/sync/stores/{store_id}/events`, the NATS cursor, and `/internal/ingest`) and the rollup read model
- The routes that compose and publish each store's configuration (store groups, releases, the config history); the tree itself is `pos-cloud-config`'s
- Webhooks, retention and PII masking, OTA release hosting, exports

## Depends on / used by

- **Depends on** (workspace crates): `blob-garage`, `erp-sap`, `link-nats`, `metrics-vm`, `pos-cloud-api`, `pos-cloud-config`, `pos-cloud-reports`, `pos-core`, `pos-country`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-ports`, `pos-proto`, `pos-providers`, `shipping-ahamove`, `shipping-grabexpress`, `store-postgres`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- `src/main.rs`: the binary
- `src/cloud.rs`: `Cloud`: ingest and the read model
- `src/http.rs`: `CloudApp` and `router`, the request middleware, and the helpers the areas share that name configuration or the registry (the generic ones are in `pos-cloud-api`); `FromRef` builds the reports' state
- `src/http/<area>.rs` and `src/http/<area>/`: an area's routes, state and handlers, one module per business area
- `pos-cloud-reports` (re-exported as `dashboard::{rollup, projection}`, the report types in `cloud`, the CSV writers in `export`): the reports and their routes; `src/dashboard/projector.rs` keeps them current
- `src/openapi.rs`, `src/openapi_admin.rs`: the generated API documents and their drift tests
- `pos-cloud-config` (re-exported as `config_tree`, `registry`, `settings`, `confirmed_values`, `qr_ordering`, `data_migrations`): the configuration tree and the publish every area writes through, the org registry and settings
- `pos-cloud-api` (re-exported as `auth`, `audit`, `paging`): console login, sessions, `ConsolePermission`, API keys, the audit trail and the error envelope

## Rules

- [ADR-0016](../../docs/adr/0016-postgres-access.md) · [ADR-0019](../../docs/adr/0019-openapi-generation.md) · [ADR-0022](../../docs/adr/0022-events-partition-strategy.md) · [ADR-0033](../../docs/adr/0033-config-tree.md) · [ADR-0034](../../docs/adr/0034-super-admin-auth.md) · [ADR-0037](../../docs/adr/0037-api-keys.md) · [ADR-0067](../../docs/adr/0067-multi-admin-console-rbac.md)

## How to test

```bash
cargo test -p pos-cloud
POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi    # after changing a documented route
just run-cloud    # needs PostgreSQL, NATS and Garage from deploy/compose.yml
```
