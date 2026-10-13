# `updater-minisign`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

Verifies that an OTA release artifact was signed by the offline key before the edge trusts it, over `ed25519-dalek` and `blake2`. It only verifies: signing stays offline, and this crate never holds a private key.

## Owns

- `Signer::verify` for minisign signatures (legacy and prehashed)

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-edge`.

## Entry points

- `src/lib.rs`: `MinisignVerifier`

## Rules

- [ADR-0047](../../../docs/adr/0047-minisign-verification.md)

## How to test

```bash
cargo test -p updater-minisign
```
