# `key-vault-keyring`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The edge's `KeyVault`: a machine's own credentials in the operating system's protected store, never a file beside the binary. It uses the `keyring` crate (Windows Credential Manager, macOS Keychain, the Linux kernel keyring). Given a vault directory, it seals them on disk instead: under a key systemd-creds keeps on Linux, and under a key protected with DPAPI on Windows. It moves any secret still in the keyring into the sealed store, and deletes the keyring copy only once the sealed one reads back the same.

## Owns

- `KeyVault` over the OS credential store
- Sealed files under a vault key: systemd-creds on Linux (ADR-0151), DPAPI on Windows (ADR-0174)
- The move from the OS keyring into sealed files, each copy proven before the keyring's goes

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `windows-protect`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-edge`.

## Entry points

- `src/lib.rs`: `KeyringVault`, `OsKeyring`, `OsVault`, `MemoryBackend`
- `src/sealed.rs`: `SealedFiles`, `SealedFirst`, `VaultKey`
- `src/protected_key.rs`: `open_or_create`, `KeyProtection`, `Dpapi` (Windows)

## Rules

- [ADR-0003](../../../docs/adr/0003-cattle-not-pets.md) · [ADR-0086](../../../docs/adr/0086-edge-keyvault-and-activation.md) · [ADR-0151](../../../docs/adr/0151-a-headless-linux-box-seals-its-secrets-with-systemd-creds.md) · [ADR-0174](../../../docs/adr/0174-a-windows-store-keeps-its-state-to-its-own-service.md)

## How to test

```bash
cargo test -p key-vault-keyring
cargo test -p key-vault-keyring --features integration    # against the real OS keyring
```
