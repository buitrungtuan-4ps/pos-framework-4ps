---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the people and access routes** (ADR-0019, ADR-0070). It covers employees, PINs, roles,
  assignments, the permission catalogue and the people publish: 14 operations on 9 paths. For each,
  it gives the parameters, the request body, the status codes it can return, and the permission it
  needs. Success bodies are described in prose, as for every other route in the document. The
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` shrinks from 151 paths to 142.
