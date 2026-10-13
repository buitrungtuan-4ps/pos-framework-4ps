# `pos-country`

**Layer** `country` · **Owners** @maintainers-architecture

## What it is

The shape of a country module: the `CountryModule` trait a crate under `countries/` implements, and the `CountryRegistry` a binary builds from the countries its Cargo features select. A country is a bundle of values (fiscalisation, locale pack, validation, retention), not an adapter.

## Owns

- The `CountryModule` trait and `CountryRegistry`
- The offline fiscalisation used where a country issues no online invoice
- Tax classes

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`.
- **Used by** (workspace crates): `pos-cloud`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`, `pos-edge`.

## Entry points

- `src/lib.rs`: `CountryModule`, `CountryRegistry`
- `src/offline.rs`: `OfflineFiscalization`
- `src/tax_class.rs`: tax classes

## Rules

- [ADR-0011](../../docs/adr/0011-country-in-hostname.md) · [ADR-0027](../../docs/adr/0027-country-modules.md) · [ADR-0105](../../docs/adr/0105-a-country-pack-is-values.md)

## How to test

```bash
cargo test -p pos-country
cargo run -q -p xtask -- countries    # every country is wired in
```
