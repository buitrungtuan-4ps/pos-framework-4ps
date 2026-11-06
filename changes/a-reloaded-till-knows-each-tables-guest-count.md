---
kind: added
---
- **A till that reloads still knows how many guests sit at each table with an order.** The store
  server's `GET /api/orders/live` now carries each order's `guest_count` (additive, omitted when
  the table was seated without one), and the till reads it with what is open, as it already heard
  it from the seat, so what a package's flows and reactions read of a table is the same before
  and after a reload.
