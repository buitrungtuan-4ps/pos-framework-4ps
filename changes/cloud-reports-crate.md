---
kind: changed
---
- **The cloud's reports are a crate of their own, `pos-cloud-reports`** (ADR-0170 decision 4, the
  reporting pilot). The rollup fold and its materialised projection, the report types, the
  reports' CSV files and their routes move out of `pos-cloud`. `pos-cloud` re-exports them under
  their old paths, mounts the routes, and keeps the projector that keeps the rollups current. The
  console's permission check, audit entry and CSV formula rule move to `pos-cloud-api`, which both
  route crates now share. Nothing a caller sees changes: every route, permission and OpenAPI entry
  is the same.
