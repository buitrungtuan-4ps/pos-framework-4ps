---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes how the console's own administrators are managed** (ADR-0019, ADR-0067). It covers 10
  operations on 9 paths:
  - an administrator's own sessions: listed, one revoked, and all the others revoked;
  - the administrator roster, with each administrator's role and status;
  - invitations: issued, listed, revoked and accepted.

  For each, it gives the parameters, the request body, the status codes it can return, and the
  permission it needs. Success bodies are described in prose. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 9 paths.
