---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes the bill routes**
  (ADR-0177): opening a bill on a table, a bill's check and its pre-bill print, a discount, a fee
  waive, splitting and merging bills, voiding and settling a bill, and a receipt copy, 10
  operations in all. A void, a discount, a waive and a settle are documented with the `403` they
  answer when an approval is missing or refused. Seventeen handlers are renamed so each
  `operationId` is unique and says what it does, with no route changed, and the 10 routes leave
  the coverage-debt list.
