---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the rest of the per-store configuration nodes** (ADR-0019, ADR-0071, ADR-0074,
  ADR-0106). It adds 6 operations on 6 paths, under the `config nodes` tag:
  - the locale, store-profile and tax publishes;
  - the capability catalogue and the capability publish;
  - the preview of any node's publish.

  For each, it gives the parameters, the request body, the status codes it can return, and the
  permission it needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses
  these 6 paths.
