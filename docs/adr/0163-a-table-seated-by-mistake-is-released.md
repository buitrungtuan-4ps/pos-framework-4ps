# ADR-0163 — A table seated by mistake is released, and a bill is never opened on nothing

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-30
· Relates to [ADR-0072](0072-floor-and-kitchen.md), [ADR-0093](0093-bill-keyed-on-order.md),
[ADR-0128](0128-a-bill-splits-and-merges.md)

## The problem

A table seated by mistake, or whose guests leave before ordering, cannot be given back to the floor.

- The table machine leaves `OCCUPIED` only by `RequestBill` (to `AWAITING_PAYMENT`) or `Transfer`.
  `Clean` is legal only from `NEEDS_CLEANING`.
- Requesting the bill opens a bill on nothing. It can never settle: `billing::assemble` refuses an
  empty set of tax bases, and the till shows the English sentence "class_bases must not be empty".
- Voiding that bill (a manager's PIN) puts the table back to `OCCUPIED`, where it started.

So the table stays taken until the store is re-provisioned. The same happens when every line on a
table was voided. The end-to-end evaluation of 2026-09-26 reproduced both over the API, and the
owner put releasing a table first in the order of work on 2026-09-30.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Settle a zero bill | Spends a gapless receipt number on a sale that did not happen, and puts a 0₫ receipt in the fiscal series |
| B | Allow `Clean` from `OCCUPIED` | Merges two different acts (clearing up after guests, and saying nobody ate), and needs a guard against cleaning a table that still has food on it anyway |
| C | **A `Release` trigger, `OCCUPIED → FREE`, only while nothing is sold** | One trigger, one edge command, one route |

## Decision

Option **C**. The owner approved it on 2026-09-30.

1. **pos-core.** The table machine gains `Release`, `OCCUPIED → FREE`. `decide_table` asks for
   nothing beyond the tables capability, as seating and clearing do: releasing moves no money.
2. **pos-edge.** `Edge::release_table` refuses unless the table's order has **no unvoided line** and
   **no bill** (`409 ORDER_NOT_EMPTY`, a new token). It writes the existing
   `sales.order.closed` (the order ends, owing nothing) and `sales.table.closed` in one transaction.
   The fold gains an arm for `sales.order.closed`, so a restart replays the release.
3. **A bill is never opened on nothing.** `open_bill_for_order` refuses an order with no unvoided
   line (`409 NOTHING_TO_BILL`, a new token), so no table waits for a payment that cannot happen.
4. **Route.** `POST /api/tables/{id}/release` (routes snapshot +1, additive).
5. **UI.** With nothing sold, the order screen offers **Release table** in place of **Take payment**.

No event, permission or protocol change. The two error tokens and the route are additive.

## Consequences accepted

- A table that is already stuck (`AWAITING_PAYMENT` on an empty bill) is recovered once: a manager
  voids the empty bill, then the table is released.
- Releasing needs no manager. It can only end an order with nothing on it, so there is nothing to
  take or give away. If a store wants it restricted, it is `sales.table.manage` under ADR-0158's
  per-person enforcement, which already lists release beside seat and clean.
- The released order stays in the log as opened and closed with no lines. Reports count it as a
  seating with no sale, not as revenue.
