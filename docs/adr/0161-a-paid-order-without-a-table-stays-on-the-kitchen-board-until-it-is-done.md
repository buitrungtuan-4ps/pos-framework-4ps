# ADR-0161 — A paid order without a table stays on the kitchen board until the kitchen is done, and its note stays with it

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-30
· Amends [ADR-0157](0157-a-guest-note-lives-in-the-stores-memory-for-the-service.md) decision 2
· Relates to [ADR-0146](0146-a-counter-store-starts-its-own-orders.md),
[ADR-0093](0093-bill-keyed-on-order.md)

## The problem

A counter store takes the money first and cooks afterwards. The kitchen board drops an order as
soon as its bill settles:

- The till takes a settled order's lines off the board when `billing.bill.settled` arrives
  (`ui/src/state/store.ts`).
- `GET /api/orders/live` lists only orders that still owe money, so a board that reloads after the
  payment does not get the ticket back.
- ADR-0157 drops a guest note when its order leaves the open orders, so the allergy note goes too.

The end-to-end probe of 2026-09-29 reproduced this. A counter order was fired with the note "Ít cay,
không hành" and paid before the kitchen bumped it. It left the board, and its note went with it. In
a counter store this happens to every order, and a delivery or QR takeaway order is paid before it
is cooked as well.

At a table the rule is right. The food reaches the guests before the bill does, and a kitchen that
never bumps relies on payment to clear its board.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep the rule, and ask counter staff to bump before they take payment | Reverses the counter's own order of work; the cashier cannot see the kitchen |
| B | Keep every paid order's unbumped lines until they are bumped | A table-service kitchen that never bumps fills its board with food already served |
| C | **Keep a paid order's unbumped lines until they are bumped, only when the order has no table** | One rule on the board and one read at the edge |

## Decision (proposed)

Option **C**. The owner approved on 2026-09-30 that the ticket stays until it is bumped or voided,
that the note stays with it, and that leftovers clear when the business day ends.

1. **The board.** A fired line stays on the kitchen board and the pass until a station bumps it or
   it is voided. If its order has no table (counter, delivery, QR takeaway), paying for it does not
   take it off. If its order has a table, it leaves when the table's bill settles, as it does today.
2. **The day ends the wait.** A paid order's unbumped lines leave the board when the business day
   they were fired in ends (the store's `cutoff_hour`), at the board's next read. A cook can bump
   one away before then.
3. **The edge serves them.** `GET /api/orders/kitchen` (new, additive) lists what the kitchen still
   has to make: every live order, plus each paid order without a table that has a fired, unbumped
   line from the current business day. The shape is that of `GET /api/orders/live`, including
   `note`. The live read, the counter list and every till screen are unchanged.
4. **The note stays with its ticket.** ADR-0157 decision 2 dropped a note "when its order leaves the
   open orders". That becomes: a note is dropped when its line is voided, or when its line is on no
   screen, which means its order no longer owes, and the line is bumped or its business day has
   ended. The 2,048-note bound, memory only, nothing on disk, in the log, in a backup or in
   telemetry: all of ADR-0157 but that rule stands.
5. **The till.** The board reads `GET /api/orders/kitchen` on load. When a bill settles, it keeps
   the lines of a paid order without a table.

No event, permission, protocol or config change. The route is additive, and the routes snapshot
grows by one.

## Before this is accepted

- The owner confirms that a table's ticket still leaves when its bill settles (option C rather than
  B). A store that takes payment at the table before cooking would want B, as a setting under
  ADR-0160's `pay_first` switch.

## Consequences accepted

- **A note is held longer**: for a paid order without a table, until the kitchen bumps its line
  or the day ends, instead of until payment. It is still memory only and still bounded, and it is
  the time the cook needs it. This is the retention change AGENTS.md §7 asks a record for.
- **A counter kitchen that never bumps** sees the day's paid orders stay on its board until the
  day ends. Today its board shows nothing once an order is paid. The bump is how the kitchen says
  it is done.
- **A ticket left from a closed day** stays on a board that has not re-read the edge since, until
  the board reloads or a cook bumps it.
