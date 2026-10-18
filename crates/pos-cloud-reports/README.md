# `pos-cloud-reports`

**Layer** `app` · **Owners** @maintainers-cloud

## What it is

The cloud's reports. Every store's events are folded into per-trading-day figures: activity counts, revenue with its fees by code and tax by component, the product mix, cash by drawer, and the devices a store has admitted. The fold is materialised, so a dashboard answers from the stored rollup rather than by scanning the log. The X/Z report, the CSV exports and the routes that serve all of it live here too. `pos-cloud` mounts the routes, and runs the projector that keeps the rollups current.

## Owns

- The fold from events to daily rollups, and the from-log computation it must always agree with
- The materialised projection and the reads that answer from it
- The report types (`DailyRollup`, `DailyRevenue`, `XzReport` and the rest)
- The reports' CSV files: rollups, revenue, fees by code, tax by component
- The routes: `/v1/stores/{store_id}/rollups/daily`, and under `/admin/stores/{store_id}/` the rollup reads, export and reset, revenue and its exports, and `reports/xz`
- Its store over PostgreSQL: the rollup table, through `store-postgres`

## Depends on / used by

- **Depends on** (workspace crates): `pos-cloud-api`, `pos-ports`, `pos-proto`, `store-postgres`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-cloud`, `pos-cloud-catalog`.

## Entry points

- `src/dashboard/rollup.rs`: `fold_event`, `daily_rollups_from_log`, `RollupWindow`
- `src/dashboard/projection.rs`: `project`, `RollupStore`, and the reads `dashboard`, `revenue`, `xz_report`
- `src/types.rs`: what the reads return
- `src/rollups.rs`, `src/revenue.rs`, `src/xz.rs`: the routes, over `ReportsState` (`src/state.rs`)
- `src/till_names.rs`: `TillNames`, the one thing the reports ask of the cloud's configuration
- `src/export.rs`: the CSV writers

## Rules

- [ADR-0036](../../docs/adr/0036-materialised-rollups.md): materialised rollups
- [ADR-0081](../../docs/adr/0081-reports-and-analytics.md): reports and analytics
- [ADR-0159](../../docs/adr/0159-a-fee-is-configuration.md): fees by code
- [ADR-0167](../../docs/adr/0167-a-till-has-its-own-cash-drawer.md): cash by drawer, and till names
- [ADR-0168](../../docs/adr/0168-a-settled-bill-records-its-tax-components.md): tax by component
- [ADR-0170](../../docs/adr/0170-repository-layout-code-naming-and-size-rules.md) decision 4: why this is a crate

## How to test

```bash
cargo test -p pos-cloud-reports
```
