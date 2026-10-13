---
kind: added
---
- **Every workspace crate has a README** (ADR-0170 decision 1). The 26 crates that had none now
  have a short one. Each says what the crate is, what it owns, which workspace crates it depends on
  and which depend on it (taken from `cargo metadata`), its entry points, the ADRs that govern it,
  and how to test it. `docs/README.md` indexes them.
