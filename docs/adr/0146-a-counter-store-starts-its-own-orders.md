# ADR-0146 — A counter store starts its own orders at the till

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0093](0093-bill-keyed-on-order.md), [ADR-0064](0064-edge-order-in.md),
[ADR-0123](0123-a-superseded-box-opens-nothing-new.md)

## The problem

A store without tables (a kiosk, a takeaway counter) cannot **begin** an order at its own till
(finding F12, `docs/ui-ux.md`). The only way to open a tableless order is the relay intake. Its orders
arrive from the cloud, so the counter screen lists and charges orders that someone else started.
`seat_table` is the till's only way to start one, and it requires the tables capability.

The domain needs nothing new. A tableless order has been billable and settleable since ADR-0093. What
is missing is a command, two routes and a screen.

## Decision

- **`POST /api/orders` opens a tableless order** for the signed-in member of staff.
  - Body: `{ "channel"?: "TAKEAWAY" | "DINE_IN" | … }`. The default is `TAKEAWAY`, and the channel
    must be one the store accepts.
  - It records `sales.order.opened` with no table.
  - The answer is `{ order_id, queue_number }`. The daily queue number is allocated exactly as it is
    for a relayed tableless order, so the guest is called by the same kind of number either way.
  - A superseded box refuses, as it refuses to seat (ADR-0123).
- **`POST /api/orders/{order_id}/lines` adds a line by order.** The edge **prices it itself, at the
  order's own channel**, from the current price book (`pos_core::menu::reprice_line`, as the intake
  does). It does not trust the price the device copied. The till's menu is priced for dine-in, and a
  takeaway line carrying the dine-in price but taxed at the takeaway rate would be a receipt that does
  not add up. A line is refused once a bill is open on the order, because that bill would not cover it.
- **The till reuses its screens.**
  - The counter screen gains **New order**.
  - The order screen opens at `/order/:id` and payment at `/order/:id/pay`.
  - It is the same flow a table uses (add → send → bill → pay), keyed by the order instead of a table.
- **Pay-first stays out.** Settling before sending takes an order off the live list, so its ticket
  would never reach the kitchen board. `pay_first_enabled` keeps having no reader until that is
  solved.

## Consequences accepted

- **Two routes are added, and none change.** `docs/snapshots/routes.txt` grows by two lines
  (ADR-0111's additive rule).
- **A walk-in line costs a menu lookup at the edge.** The price book is already in memory, so the
  cost is small.
- **An empty order is invisible until its first line.** The live list requires one line, as it
  always has. The till keeps the new order's id and goes straight to it.
