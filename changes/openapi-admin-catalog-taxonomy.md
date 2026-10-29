---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the catalog's taxonomies** (ADR-0019, ADR-0066, ADR-0130). It covers 15 operations on
  10 paths, under a new `catalog taxonomy` tag: item categories and sub-categories, display
  categories and sub-categories, and courses, each listed, created and updated. For each, it gives
  the parameters, `If-Match` on every update, the request body, the status codes it can return, and
  the permission it needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses
  these 10 paths.
