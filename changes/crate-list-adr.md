---
kind: changed
---
- **ADR-0175: the crate list follows what a move can reach.** It supersedes ADR-0170 decision 4's
  crate list on three points:
  - The edge's billing, shift and floor commands stay inside `pos-edge-app`.
  - Catalog and fees become one crate, `pos-cloud-catalog`.
  - Integrations and admin stay in `pos-cloud` until a written sub-state design says how they can
    leave.

  Nothing a store or a console user sees changes.
