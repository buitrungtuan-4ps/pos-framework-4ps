---
kind: changed
---
- **Five of the longest functions are split by meaning** (ADR-0170 decision 7, the reporting
  pilot): the settings register (`pos-proto`), how the edge applies a synced config document,
  scheduling a release, checking a store's locale, and bumping its lease (`pos-cloud`). Each kept
  its tests, gained a mutation check, and left the size baseline. Nothing a store, the console or
  an integrator sees changes.
