---
kind: added
---
- **A Windows store can seal its secrets under a DPAPI-protected vault key** (ADR-0174).
  - **The key:** given `POS_EDGE_VAULT_KEY` and `POS_EDGE_VAULT_DIR`, the store server makes its
    vault key on the first start, protects it with DPAPI in machine scope, and seals its secrets
    under it as a Linux box does.
  - **The move:** at every start it moves any secret still in the OS keyring into the sealed vault.
    A keyring copy is deleted only once the sealed copy reads back the same, on Linux as well. The
    installer sets these paths in a later change; until then nothing changes on a Windows box.
