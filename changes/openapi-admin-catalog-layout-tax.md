---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the rest of the catalog** (ADR-0019, ADR-0066, ADR-0074, ADR-0104, ADR-0127). It covers
  13 operations on 8 paths:
  - a till's layout buttons, under a new `till layout` tag;
  - modifier groups, under a new `modifiers` tag;
  - tax classes and the tenant's tax-rate table, under a new `tax` tag;
  - the menu publish, under `menus`.

  For each, it gives the parameters, `If-Match` on every update, the request body, the status codes
  it can return, and the permission it needs. No description carries an example rate. The
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 8 paths, and no catalog
  path is left on it.
