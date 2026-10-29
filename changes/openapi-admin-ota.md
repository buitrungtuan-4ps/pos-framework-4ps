---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes OTA rollouts and release hosting** (ADR-0019, ADR-0052, ADR-0078, ADR-0088, ADR-0141). It
  covers 9 operations on 7 paths, under two new tags. `ota rollout` covers publishing, halting and
  reading a store's rollout, and publishing and reading its placement. `release hosting` covers
  uploading a signed release, having the cloud fetch one, reading which targets a release is hosted
  for, and downloading a store's Windows installer. For each, it gives the parameters, the request
  body, the status codes it can return, and the permission it needs. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 7 paths.
