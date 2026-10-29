---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the store lease** (ADR-0019, ADR-0108, ADR-0110, ADR-0114). It covers 4 operations on 4
  paths, under a new `lease` tag: reading a store's lease generation, bumping it to make a different
  machine the store, and settling and retiring a handover. For each, it gives the parameters, the
  required `If-Match` on the bump, the request body, the status codes it can return, and the
  permission it needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these
  4 paths.
