---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the operations reads and the audit trail** (ADR-0019, ADR-0068, ADR-0069, ADR-0073,
  ADR-0078). It covers 8 operations on 8 paths:
  - the fleet list and one store's fleet detail;
  - alerts: listed, acknowledged and resolved;
  - background-task health;
  - reconciliation runs;
  - the audit trail.

  For each, it gives the parameters, the status codes it can return, and the permission it needs.
  Success bodies are described in prose. They are filed under the `fleet` tag and two new tags,
  `operations` and `audit`. The `UNDOCUMENTED` list in `crates/pos-cloud/src/openapi_admin.rs`
  loses these 8 paths.
