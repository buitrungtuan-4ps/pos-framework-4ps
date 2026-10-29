---
kind: added
---
- **The till's OpenAPI document (`docs/openapi-edge.json`) now describes the table commands, the
  pre-bill prints and the sold-out marks** (ADR-0177): seating, cleaning, releasing and moving a
  table, printing a table's or an order's pre-bill, and marking a menu item sold out or back on
  sale, 8 operations in all. The 8 routes leave the coverage-debt list.
