---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes every `/admin` route** (ADR-0019, ADR-0079, ADR-0177). The last 7 paths, 16
  operations under a new `inventory` tag, are a tenant's ingredients, recipes and suppliers, and the
  inventory publish. No recipe amount appears in any description. `pos-cloud-inventory` gains
  `utoipa`, as ADR-0177 decided. The `UNDOCUMENTED` list is empty and is deleted, and the test that
  allowed it now fails any `/admin` route that is not documented.
