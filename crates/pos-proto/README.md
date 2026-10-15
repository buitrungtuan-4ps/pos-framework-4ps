# `pos-proto`

**Layer** `core` · **Owners** @maintainers-architecture

## What it is

Everything that crosses a boundary between `pos_edge`, `pos_cloud` and the outside: the event envelope and catalogue, money and quantity value types, identifiers, wire enums, the error envelope, the settings register, and `PROTOCOL_VERSION`. It also holds the two synchronous determinism traits, `ClockSource` and `IdGenerator`. It performs no I/O.

## Owns

- The event catalogue and its snapshot, `docs/snapshots/events.txt`
- The settings register, rendered to `docs/snapshots/settings.txt` and `docs/configuration.md`
- Money (`currency_code` + integer `amount_minor`), quantities, identifiers, time types
- `PROTOCOL_VERSION` and version negotiation

## Depends on / used by

- **Depends on** (workspace crates): no workspace crate.
- **Used by** (workspace crates): `cloud-sync-http`, `erp-sap`, `link-nats`, `metrics-vm`, `minimal-edge`, `pos-cloud`, `pos-cloud-api`, `pos-cloud-catalog`, `pos-cloud-config`, `pos-cloud-devices`, `pos-cloud-floor`, `pos-cloud-inventory`, `pos-cloud-reports`, `pos-contract-tests`, `pos-core`, `pos-country`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-edge`, `pos-edge-app`, `pos-edge-printing`, `pos-fakes`, `pos-ports`, `pos-print-agent`, `pos-providers`, `printer-escpos`, `shipping-ahamove`, `shipping-grabexpress`, `store-postgres`, `store-sqlite`.

## Entry points

- `src/lib.rs`: `PROTOCOL_VERSION` and the re-exports
- `src/events.rs`: the event catalogue: one payload type per event
- `src/envelope.rs`: the event envelope
- `src/settings.rs`: `register()`, every setting a store may run differently
- `src/money.rs`, `src/ids.rs`, `src/time.rs`: the value types
- `src/snapshot.rs`: renders the event snapshot

## Rules

- [ADR-0010](../../docs/adr/0010-naming-standard.md) · [ADR-0013](../../docs/adr/0013-async-strategy.md) · [ADR-0024](../../docs/adr/0024-protocol-version-negotiation.md) · [ADR-0160](../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)

## How to test

```bash
cargo test -p pos-proto
just snapshot    # regenerate the committed snapshots after a change
```
