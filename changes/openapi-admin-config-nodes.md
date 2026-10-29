---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the per-store configuration nodes a console reads and publishes one at a time**
  (ADR-0019, ADR-0057, ADR-0080, ADR-0111, ADR-0145). It covers a read and a publish, 12 operations
  in all, on 6 paths, under a new `config nodes` tag: sales channels, accepted payment methods,
  second origins, QR ordering guardrails, event retention, and delivery-marketplace policies. For
  each, it gives the parameters, the request body, the status codes it can return, and the
  permission it needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses
  these 6 paths.
