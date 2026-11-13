---
kind: added
---
- **The cloud's environment key is a port.** `KeyKeeper` wraps and unwraps tenant keys under a
  versioned environment key, with a shared contract suite; the file keeper (`integration_secret`,
  development only) is its first adapter. Nothing seals under tenant keys yet (ADR-0217).
