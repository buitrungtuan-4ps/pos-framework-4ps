---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes a store's configuration tree** (ADR-0019, ADR-0033, ADR-0095, ADR-0169). It covers 5
  operations on 5 paths, under the `config` tag:
  - the effective configuration;
  - replacing one layer;
  - the version history and one past version;
  - the rollback.

  For each, it gives the parameters, the required `If-Match` on both writes, the request body, the
  status codes it can return, how the reads are redacted for the reader, and the permission it
  needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 5 paths.
