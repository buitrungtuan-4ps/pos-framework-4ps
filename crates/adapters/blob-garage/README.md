# `blob-garage`

**Layer** `adapter` · **Owners** @maintainers-cloud

## What it is

The cloud's `BlobStore` over Garage or any S3-compatible server. It hand-rolls SigV4 signing and HTTP/1.1 rather than carrying an S3 SDK. `pos-cloud` uses it to host OTA release artifacts.

## Owns

- `BlobStore`: put, get, delete, prefix listing
- SigV4 request signing, checked against AWS's published vector

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/store.rs`: `S3Blobs`
- `src/sign.rs`: SigV4

## Rules

- [ADR-0007](../../../docs/adr/0007-in-house-vs-dependency.md) · [ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md) · [ADR-0088](../../../docs/adr/0088-ota-artifact-hosting.md)

## How to test

```bash
cargo test -p blob-garage
S3_ENDPOINT=… cargo test -p blob-garage --features integration    # against a live S3 server
```
