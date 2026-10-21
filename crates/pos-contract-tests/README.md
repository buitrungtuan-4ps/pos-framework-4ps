# `pos-contract-tests`

**Layer** `testing` · **Owners** @maintainers-architecture

## What it is

The suites that make an adapter swappable as a tested fact: one per port. An adapter's own test file implements a harness from here and calls the port's suite, so `store-sqlite`, `store-postgres` and the in-memory fakes are held to the same behaviour.

## Owns

- One suite per port
- The country conformance runner (`country_conformance`): golden bills a country module's values must produce, through `pos_core::billing`
- The harness traits an adapter implements to be tested
- Shared fixtures

## Depends on / used by

- **Depends on** (workspace crates): `pos-core`, `pos-ports`, `pos-proto`.
- **Used by** (workspace crates): `pos-fakes`; in tests, `blob-garage`, `cloud-sync-http`, `erp-sap`, `key-vault-keyring`, `link-nats`, `metrics-vm`, `pos-cloud`, `pos-cloud-reports`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-edge`, `printer-escpos`, `shipping-ahamove`, `shipping-grabexpress`, `store-postgres`, `store-sqlite`, `updater-minisign`.

## Entry points

- `src/harness.rs`: the harness an adapter implements
- `src/<port>.rs`: one suite per port
- `src/fixtures.rs`: shared test data

## Rules

- [ADR-0026](../../docs/adr/0026-port-shapes.md) · [ADR-0131](../../docs/adr/0131-a-chained-event-log.md)

## How to test

```bash
cargo test -p pos-contract-tests
cargo test -p <adapter> --test contract    # a suite runs from the adapter that uses it
```
