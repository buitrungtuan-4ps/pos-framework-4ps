---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the country and locale master data** (ADR-0019, ADR-0074, ADR-0105): `GET
  /admin/countries` and `GET /admin/locales`, under a new `countries` tag. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 2 paths.
