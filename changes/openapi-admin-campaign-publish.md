---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes campaign publishing, scheduled publishes and its own route** (ADR-0019, ADR-0077). It
  covers 6 operations on 6 paths:
  - Under `campaigns`: publishing a tenant's campaigns to a store, previewing that publish, and
    scheduling it for a later instant.
  - Under a new `scheduled publishes` tag: listing a store's pending publishes and cancelling one.
  - Under a new `openapi` tag: the document's own route, which needs no session.

  For each, it gives the parameters, the request body, the status codes it can return, and the
  permission it needs. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` now holds
  only the 17 floor, kitchen and inventory paths, whose crates have no OpenAPI dependency yet.
