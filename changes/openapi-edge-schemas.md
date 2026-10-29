---
kind: added
---
- **The till's OpenAPI document gives every answer and request body its schema.**
  `docs/openapi-edge.json` names the type of each success answer and each request body on the
  edge's `/api` routes, and `ui/src/api/generated/edge.d.ts` is regenerated from it. Only
  `GET /api/print/jobs` waits, for `pos-ports`' `openapi` feature (ADR-0180).
