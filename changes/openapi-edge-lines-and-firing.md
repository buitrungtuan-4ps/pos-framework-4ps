---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes the line commands and
  firing** (ADR-0177): adding a line to a table, changing a line's quantity, voiding a line,
  firing a line, a course or a whole order, and a kitchen station's bump, 7 operations in all. A
  void is documented with the `403` it answers when a manager's approval is missing or refused, and
  a fire with the `ticket_print` outcome it reports. The 7 routes leave the coverage-debt list.
