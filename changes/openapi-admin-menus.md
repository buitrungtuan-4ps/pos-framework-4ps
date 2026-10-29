---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes menus** (ADR-0019, ADR-0066). It covers 10 operations on 6 paths, under a new `menus`
  tag:
  - menus listed, created and updated, with a parent each may inherit from;
  - their sections listed, created and updated;
  - an item's placement on a menu listed, added, changed and removed.

  For each, it gives the parameters, `If-Match` on every update, the request body, the status codes
  it can return, and the permission it needs. Reading placements needs `console.reports.revenue`,
  because they carry prices. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs`
  loses these 6 paths.
