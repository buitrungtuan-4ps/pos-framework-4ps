---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes the session and the
  configuration reads** (ADR-0177): signing in and out and the session read, the published floor,
  menu, layout, locale, reason codes, integrations and printers, and the outbox and takings status,
  12 operations in all. A configuration read is documented with its `304` for an `If-None-Match`
  that names its ETag. Nine handlers are renamed so each `operationId` is unique and says what it
  reads, with no route changed, and they leave the coverage-debt list.
