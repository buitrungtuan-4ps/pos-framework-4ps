# `cloud-sync-http`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The store's one request/response channel to its cloud. `HttpCloudSync` does the first-boot activation exchange and fetches OTA artifacts; `HttpLink` delivers events over the same transport. The socket is behind `HttpTransport`, so the mapping is tested against a stub.

## Owns

- `CloudSync`: activation and OTA artifact fetch
- `MessageLink` over HTTPS (`HttpLink`)
- The box's half of being claimed from the console (`HttpClaim`)

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-edge`.

## Entry points

- `src/client.rs`: `HttpCloudSync`
- `src/link.rs`: `HttpLink`
- `src/claim.rs`: `HttpClaim`
- `src/wire.rs`: `HttpTransport`, `TlsHttpTransport`

## Rules

- [ADR-0050](../../../docs/adr/0050-activation-code-exchange.md) · [ADR-0054](../../../docs/adr/0054-edge-cloud-http-client.md) · [ADR-0143](../../../docs/adr/0143-the-device-credential-syncs-and-events-travel-over-https.md)

## How to test

```bash
cargo test -p cloud-sync-http
```
