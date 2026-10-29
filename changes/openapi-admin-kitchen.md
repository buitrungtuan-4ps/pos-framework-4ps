---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes a store's kitchen** (ADR-0019, ADR-0072, ADR-0177). It covers 7 operations on 4 paths,
  under a new `kitchen` tag: listing, reading, creating and updating stations, and listing, creating
  and removing routing rules. For each, it gives the parameters, the required `If-Match` on an
  update, the request body, the status codes it can return, and the permission it needs. The
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 4 paths.
