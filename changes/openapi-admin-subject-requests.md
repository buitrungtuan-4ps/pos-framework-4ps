---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes the data-subject request routes** (ADR-0019, ADR-0076). It covers the 3 operations on
  `/admin/subjects/{subject_id}`, under a new `subject requests` tag: look up, export and erase.
  Each one acts on a single subject of a single tenant, needs the owner-only
  `console.subjects.manage`, and is audited without personal values. The tag states the process
  these routes serve: confirm the lawful basis and the subject's identity first, escalate an
  EU-resident request to the Data Protection contact, and clear any cross-border transfer of an
  export. No description carries example personal data. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 3 paths.
