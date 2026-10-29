---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes a store's floor** (ADR-0019, ADR-0072, ADR-0177). It covers 10 operations on 6 paths,
  under a new `floor` tag: listing, reading, creating and updating areas and tables, publishing the
  floor and kitchen, and the printable table QR sheet. For each, it gives the parameters, the
  required `If-Match` on an update, the request body, the status codes it can return, and the
  permission it needs. `pos-cloud-floor` gains `utoipa`, as ADR-0177 decided, and the
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 6 paths.
