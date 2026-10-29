---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the rest of a store's reports** (ADR-0019, ADR-0036, ADR-0081). It adds 6 operations on
  6 paths, under the `reports` tag:
  - the daily activity read and its CSV export;
  - the daily revenue read and its CSV export;
  - the X or Z report;
  - the rollup reset.

  For each, it gives the window parameters, the status codes it can return, and the permission it
  needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 6 paths.
