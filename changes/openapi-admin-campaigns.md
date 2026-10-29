---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes campaign authoring and vouchers** (ADR-0019, ADR-0077). It covers 7 operations on 3
  paths, under a new `campaigns` tag: campaigns listed, read, created, updated and deleted, and a
  voucher campaign's codes minted and listed. For each, it gives the parameters, `If-Match` on the
  update, the request body, the status codes it can return, and the permission it needs. The
  `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs` loses these 3 paths. Publishing and
  scheduling campaigns are left for the configuration slices.
