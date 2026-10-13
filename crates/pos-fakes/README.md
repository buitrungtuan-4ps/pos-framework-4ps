# `pos-fakes`

**Layer** `testing` · **Owners** @maintainers-architecture

## What it is

In-memory implementations of the ports. They let `pos-core`, `pos-edge` and `pos-cloud` tests run with no database, network or hardware, and `examples/minimal-edge` boots the store on them. They pass the same contract suites as the real adapters, so a domain test does not test the wrong thing.

## Owns

- `FakeStore` (event, config, device, subject and intake stores) and `FakeTx`
- `FakeClock` and `FakeIdGenerator`
- Fake printer, payment terminal, fiscalisation, ERP, shipping and delivery vendors

## Depends on / used by

- **Depends on** (workspace crates): `pos-contract-tests`, `pos-ports`, `pos-proto`.
- **Used by** (workspace crates): `minimal-edge`; in tests, `key-vault-keyring`, `pos-cloud`, `pos-cloud-api`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-edge`, `printer-escpos`, `store-sqlite`, `updater-minisign`.

## Entry points

- `src/store.rs`: `FakeStore`, `FakeTx`
- `src/determinism.rs`: `FakeClock`, `FakeIdGenerator`
- `src/devices.rs`, `src/vendors.rs`: the device and vendor fakes
- `tests/contract.rs`: runs the contract suites against the fakes

## Rules

- [ADR-0026](../../docs/adr/0026-port-shapes.md)

## How to test

```bash
cargo test -p pos-fakes
```
