---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes how a box is activated** (ADR-0019, ADR-0050, ADR-0065, ADR-0148). It covers 6
  operations on 5 paths, under a new `box activation` tag:
  - a store's device slots: listed, created and updated;
  - issuing and cancelling an activation code;
  - binding the code a box with no store shows.

  For each, it gives the parameters, `If-Match` on the slot update, the request body, the status
  codes it can return, and the permission it needs. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 5 paths.
