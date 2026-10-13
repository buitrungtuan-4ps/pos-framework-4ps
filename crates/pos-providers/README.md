# `pos-providers`

**Layer** `port` · **Owners** @maintainers-architecture

## What it is

What a vendor is to the rest of the system. Each vendor adapter exports one `ProviderDescriptor`: its id, family, countries and the settings schema a tenant fills in. The cloud and the edge build a `Registry` from the descriptors compiled in, and the console draws its connection form from the schema. It holds no secret values.

## Owns

- `ProviderDescriptor` and its settings schema
- `Registry`
- Checking an operator's settings against the schema

## Depends on / used by

- **Depends on** (workspace crates): `pos-proto`.
- **Used by** (workspace crates): `erp-sap`, `pos-cloud`, `shipping-ahamove`, `shipping-grabexpress`.

## Entry points

- `src/lib.rs`: `ProviderDescriptor`, `Family`
- `src/registry.rs`: `Registry`
- `src/settings.rs`, `src/fields.rs`: the settings schema

## Rules

- [ADR-0153](../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)

## How to test

```bash
cargo test -p pos-providers
```
