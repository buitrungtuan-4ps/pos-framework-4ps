# `shipping-ahamove`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The Ahamove `ShippingDispatch`: create a delivery, cancel it, and track it, mapping the courier's status vocabulary to a `ShipmentStatus`. The socket is behind `CourierTransport`, so the contract suite runs against a stub courier. It exports a provider descriptor for the vendor catalogue.

## Owns

- `ShippingDispatch`: create, cancel, track
- Its `ProviderDescriptor`

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`, `pos-providers`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/client.rs`: `HttpAhamove`
- `src/wire.rs`: `CourierTransport`, `TlsCourierTransport`

## Rules

- [ADR-0058](../../../docs/adr/0058-shipping-adapters.md) · [ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)

## How to test

```bash
cargo test -p shipping-ahamove
```
