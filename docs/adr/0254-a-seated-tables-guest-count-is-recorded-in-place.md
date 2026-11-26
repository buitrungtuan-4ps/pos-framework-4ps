# ADR-0254 — A seated table's guest count is recorded in place

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (phase 10 open item O10: a flow refusal names the step and lets the
cashier supply it) · Adds an act and an event (`AGENTS.md` §7) · Follows
[ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md) and
[ADR-0244](0244-a-flows-guards-apply-to-the-orders-it-governs.md)

## Context

A store's flow can ask for the guest count before the first dish (a step on `sales.table.seat`
whose `done_when` reads `order.guest_count`). The count is recorded only when the table is seated,
on `sales.order.opened`. When the cashier seats a table without it, the edge refuses the first
line with `FLOW_PREREQUISITE_UNMET`, and the till can only say "give the table back and seat it
again", which is impossible once anything is sold and closes and re-opens the order otherwise.
Nothing records a count on a table already seated.

## Options considered

1. **Release and seat again.** Today's advice; loses the order once a line exists, and writes two
   orders for one party.
2. **Let the seat act run on a seated table.** Changes what `sales.table.seat` means and its
   table state machine, and a second `sales.order.opened` for one order breaks every reader.
3. **A new act and event that record the count on the table's open order.** Additive.

## Decision

1. **Option 3.** A new catalogue act `sales.table.count_guests` (effect `EDGE_WRITE`, parameters
   `table_id` and `guest_count`, permission `sales.table.manage`, the seat's own) and its edge route
   `POST /api/tables/{id}/guests` with the body `{"guest_count": <n>}`, `n` at least 1 (the seat's own range otherwise).
2. **A new event `sales.order.guests_counted`** (version 1): `order_id` and `guest_count`. It carries
   no person, so it holds no PII. It is written on the table's open order while the table is seated
   or waiting for payment; a free or cleaning table is refused as the seat's state machine refuses.
3. **The latest count wins.** It replaces any count `sales.order.opened` carried, for the edge's flow
   facts (`order.guest_count`), the kitchen board and any report. `sales.order.opened` is unchanged.
4. **Flows need no change.** The step stays the seat's; the edge re-evaluates its `done_when` over
   the order's facts, so once a count is recorded the waiting line is accepted. No step is marked
   done by the act itself.
5. **The till asks in place.** When a line waits on a step whose act is `sales.table.seat`, the till
   names it in words and offers the count there; any other missing step is named by its act's words,
   never by its step id or act id.

## Consequences accepted

- One more act, route and event in the published snapshots; older readers skip the event, so a
  cloud report that predates it keeps the seat's count until it folds the new one.
- A count can be corrected as often as the cashier records it; the events keep every value and who
  recorded it.
