---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the organisation registry** (ADR-0019, ADR-0065). It covers 9 operations on 6 paths,
  under a new `registry` tag: tenants, brands and stores, each listed, created and updated. For
  each, it gives the parameters, `If-Match` on every update, the request body, the status codes it
  can return, and the permission it needs. Success bodies are described in prose. The
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 6 paths.
