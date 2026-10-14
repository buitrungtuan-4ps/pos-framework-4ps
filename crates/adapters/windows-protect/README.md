# `windows-protect`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

Windows' own protection for a store box's state: DPAPI in machine scope for the vault key the edge's
secrets are sealed under. Only this machine can decrypt it, and the `data\` directory's ACL keeps
other local accounts from reading the file it is in. It is the one crate where the workspace lifts
`unsafe_code` for FFI, on Windows only.

## Owns

- `protect_for_machine` and `unprotect` (DPAPI, `CRYPTPROTECT_LOCAL_MACHINE`), on Windows
- `ProtectError`, everywhere

## Depends on / used by

- **Depends on** (workspace crates): no workspace crate.
- **Used by** (workspace crates): `key-vault-keyring`.

## Entry points

- `src/lib.rs`: `ProtectError`, and the Windows re-exports
- `src/dpapi.rs`: `protect_for_machine`, `unprotect`

## Rules

- [ADR-0174](../../../docs/adr/0174-a-windows-store-keeps-its-state-to-its-own-service.md)
- Every `unsafe` block is one call wide and carries a `// SAFETY:` comment
  (`clippy::undocumented_unsafe_blocks` is denied here).

## How to test

```bash
cargo test -p windows-protect   # on Windows: protect, unprotect, and damaged bytes refused
```

On Linux the crate is empty but for `ProtectError`, and its one test runs there too.
