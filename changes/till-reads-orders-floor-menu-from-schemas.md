---
kind: changed
---
- **The till's order, floor, menu, bill, kitchen and layout types come from the OpenAPI
  document.** `ui/src/api/schemas.ts` names the generated edge schemas, and forty-one
  hand-mirrored types in `ui/src/api/types.ts` are aliases of them. The bytes on the wire are
  unchanged.
