---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the media library and the translation grid** (ADR-0019, ADR-0042, ADR-0043, ADR-0075).
  It covers 10 operations on 8 paths, under new `media` and `translations` tags:
  - images listed, uploaded, served as thumbnail and detail, and deleted;
  - the grid read and replaced, exported as CSV, and imported after a dry run.

  For each, it gives the parameters, `If-Match` on the grid replace, the request body (an image or
  a CSV as the raw body), the status codes it can return, and the permission it needs. The
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 8 paths.
