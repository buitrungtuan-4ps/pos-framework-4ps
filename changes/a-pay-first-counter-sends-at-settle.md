---
kind: added
upgrade_note: "A store with `pay_first_enabled` now acts on it: settling an order with no table writes `sales.order_line.fired` for its unsent lines in the settle's own transaction, so the kitchen sees the order once it is paid. Before, nothing read the flag; a store that already had it on, from the café, fast-food or bakery-retail preset, starts firing at settle on upgrade. The console offers the switch again."
---
- **A pay-first counter sends the order to the kitchen when it is paid.** On a store that takes
  the money first, paying a counter order sends its unsent lines to the kitchen in the same step,
  so the cashier no longer sends them before taking payment (ADR-0222). A line no kitchen station
  takes stays unsent. The console offers the pay-first switch again.
