---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the integrator credentials and destinations** (ADR-0019, ADR-0032, ADR-0037). It covers
  7 operations on 5 paths, under a new `integrator access` tag:
  - a tenant's scoped API keys: provisioned, listed and revoked;
  - webhook endpoints: registered, listed, deleted and re-enabled.

  For each, it gives the parameters, the request body, the status codes it can return, and the
  permission it needs. Success bodies are described in prose. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 5 paths.
