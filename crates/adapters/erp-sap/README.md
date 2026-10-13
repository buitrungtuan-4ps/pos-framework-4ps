# `erp-sap`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The SAP `ErpSink`: a nightly posting of a trading day's revenue and consumption to a REST ERP API. The socket is behind `ErpTransport`; building the batch and mapping the ERP's status to `PortError` are pure, so the contract suite runs against a stub ERP. It exports a provider descriptor for the vendor catalogue.

## Owns

- `ErpSink`: `post` and `posted`
- Its `ProviderDescriptor`

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`, `pos-providers`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/client.rs`: `HttpSapErp`
- `src/wire.rs`: `ErpTransport`, `TlsErpTransport`

## Rules

- [ADR-0058](../../../docs/adr/0058-shipping-adapters.md) · [ADR-0059](../../../docs/adr/0059-erp-adapter.md) · [ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)

## How to test

```bash
cargo test -p erp-sap
```
