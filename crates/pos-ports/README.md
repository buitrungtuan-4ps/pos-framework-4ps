# `pos-ports`

**Layer** `port` · **Owners** @maintainers-architecture

## What it is

The boundaries between the framework and every external system: database, broker, printer, terminal, marketplace, tax authority. Each is one trait, and every external system is one implementation of one of them. Eighteen are async traits defined here; `ClockSource` and `IdGenerator` are re-exported from `pos-proto`. `PortName::ALL` lists all twenty.

## Owns

- The port traits and their written contracts
- `PortError`, the one error type every port returns
- Object-safe mirrors (`Dyn…`) where a family is chosen at runtime
- `TxContext`, the transaction an event is written inside

## Depends on / used by

- **Depends on** (workspace crates): `pos-proto`.
- **Used by** (workspace crates): `blob-garage`, `cloud-sync-http`, `erp-sap`, `key-vault-keyring`, `link-nats`, `metrics-vm`, `pos-cloud`, `pos-cloud-api`, `pos-cloud-config`, `pos-cloud-reports`, `pos-contract-tests`, `pos-country`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-edge`, `pos-fakes`, `pos-print-agent`, `pos-render`, `printer-escpos`, `shipping-ahamove`, `shipping-grabexpress`, `store-postgres`, `store-sqlite`, `updater-minisign`.

## Entry points

- `src/lib.rs`: the list of ports and the re-exports
- `src/error.rs`: `PortError` and `PortName`
- `src/event_store.rs`, `src/tx.rs`: the event store and its transaction
- `src/<port>.rs`: one file per port

## Rules

- [ADR-0006](../../docs/adr/0006-ports-and-adapters.md) · [ADR-0013](../../docs/adr/0013-async-strategy.md) · [ADR-0021](../../docs/adr/0021-corrected-port-list.md) · [ADR-0026](../../docs/adr/0026-port-shapes.md)

## How to test

```bash
cargo test -p pos-ports
```
