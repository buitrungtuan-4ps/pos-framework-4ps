---
kind: added
---
- **The first crates of phase 6 are measured** (ADR-0170 decision 4). `docs/metrics/crates-2026-10.md`
  records what `pos-cloud-config`, `pos-cloud-inventory` and `pos-cloud-devices` cost and changed:
  build and test times before and after with a target directory per side, what changing one thing
  in each area means reading, what CI runs, the crate sizes, what was found, and an order for the
  remaining crates. Decision gate 2 is taken on it: continue, one area at a time, where the boundary
  is clean.
