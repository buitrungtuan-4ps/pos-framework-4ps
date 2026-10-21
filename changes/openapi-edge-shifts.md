---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes the cash drawer**
  (ADR-0177): the drawer list and the current shift, opening, counting and closing a shift, a batch
  close, cash paid in and out, and opening the drawer outside a sale, 9 operations in all. A close
  is documented with the JSON body its `409 VARIANCE_REASON_REQUIRED` carries. Eight handlers are
  renamed so each `operationId` is unique and says what it does, with no route changed, and the 9
  routes leave the coverage-debt list.
