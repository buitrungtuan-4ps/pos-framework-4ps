---
kind: changed
---
- **The cloud's shared route base is a crate of its own, `pos-cloud-api`** (ADR-0170 decision 4,
  the reporting pilot). Console sessions, roles and permissions, API keys and device credentials,
  the audit trail, paging and the error envelope move out of `pos-cloud` unchanged. `pos-cloud`
  re-exports `auth`, `audit` and `paging`, so their paths still work. Nothing a caller sees
  changes: every route, permission and OpenAPI entry is the same.
