---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes the order reads and the
  counter's commands** (ADR-0177): the live, kitchen, open and awaiting-confirmation order lists,
  the order and table checks, a table's state, today's settled bills, opening a counter order and
  adding a line to it, confirming or refusing a guest's order, and opening a bill on an order, 13
  operations in all. Each command is documented with the refusals the edge answers: `400` for an
  id that is not a ULID, `403`, `409` with its `pos-error-reason`, `503` and `500`. Twelve handlers
  are renamed so each `operationId` is unique and says what it does, with no route changed, and
  the 13 routes leave the coverage-debt list.
