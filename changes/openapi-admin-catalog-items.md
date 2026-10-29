---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the item master** (ADR-0019, ADR-0066, ADR-0075). It covers 6 operations on 5 paths,
  under a new `catalog items` tag:
  - items listed, created and updated;
  - the item CSV export, its import dry run, and the import itself.

  For each, it gives the parameters, `If-Match` on the update, the request body (the import's is the
  export's CSV), the status codes it can return, and the permission it needs. No description carries
  a sample row or a price. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses
  these 5 paths.
