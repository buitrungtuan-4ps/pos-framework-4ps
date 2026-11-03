---
kind: changed
---
- **A drawer that has been counted takes no more cash**
  ([ADR-0186](../docs/adr/0186-a-drawer-is-counted-by-denomination.md) decision 4). Between
  **Enter count** and **Close**, a cash settle, a cash void after payment or a cash split part into
  that drawer is refused with `409 DRAWER_COUNTED`, and the till says "This drawer is being closed.
  Take this payment at another till, or by card." Card and other tenders still settle. Before, a
  sale rung in that window raised the expected amount after the count, so the close read short.
