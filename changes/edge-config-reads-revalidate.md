---
kind: added
---
- **The edge says what changed, and answers an unchanged configuration read with a `304`**
  (ADR-0173). `GET /api/floor`, `/api/menu`, `/api/layout`, `/api/locale` and `/api/reason-codes`
  each carry a weak `ETag` and `Cache-Control: private, no-cache`. A request whose `If-None-Match`
  names the current tag gets a `304` with no body. The `config_applied` frame on `/ws` names the
  top-level configuration nodes that changed in `changed_nodes`, except on the first configuration
  an edge applies after it starts. Both are additive: a till that sends no `If-None-Match` and
  ignores the new field works as before.
