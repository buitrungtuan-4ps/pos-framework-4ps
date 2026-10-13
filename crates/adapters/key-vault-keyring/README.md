# `key-vault-keyring`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The edge's `KeyVault`: a machine's own credentials in the operating system's protected store, never a file beside the binary. It uses the `keyring` crate (Windows Credential Manager, macOS Keychain, the Linux kernel keyring), and on a headless Linux box seals them under a systemd-creds vault key.

## Owns

- `KeyVault` over the OS credential store
- Sealed files under a vault key on headless Linux

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-edge`.

## Entry points

- `src/lib.rs`: `KeyringVault`, `OsKeyring`, `OsVault`, `MemoryBackend`
- `src/sealed.rs`: `SealedFiles`, `VaultKey`

## Rules

- [ADR-0003](../../../docs/adr/0003-cattle-not-pets.md) · [ADR-0086](../../../docs/adr/0086-edge-keyvault-and-activation.md) · [ADR-0151](../../../docs/adr/0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md)

## How to test

```bash
cargo test -p key-vault-keyring
cargo test -p key-vault-keyring --features integration    # against the real OS keyring
```
