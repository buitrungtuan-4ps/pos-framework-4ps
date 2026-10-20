---
kind: added
---
- **The console's OpenAPI document (`docs/openapi-admin.json`, `GET /admin/openapi.json`) now
  describes device onboarding** (ADR-0019, ADR-0041, ADR-0100, ADR-0112, ADR-0165). It covers 7
  operations on 7 paths:
  - under a new `device onboarding` tag: the approval queue, approve and reject, creating a till,
    and the devices publish;
  - under the `printers` tag: a printer's print agent and its cash-drawer mark.

  For each, it gives the parameters, `If-Match` on the printer writes, the request body, the status
  codes it can return, and the permission it needs. The `UNDOCUMENTED` list in
  `crates/pos-cloud/src/openapi_admin.rs` loses these 7 paths.
