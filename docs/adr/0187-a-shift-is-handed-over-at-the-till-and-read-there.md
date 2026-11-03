# ADR-0187 — A shift is handed over at the till, and the till reads its X

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0167](0167-a-till-has-its-own-cash-drawer.md) (what a handover is) and
[ADR-0165](0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)
(decision 5, the X report it deferred) · Settles the X half of `docs/roadmap.md` D10 · Relates to
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0184](0184-a-settled-bill-is-voided-whole-not-refunded.md),
[ADR-0186](0186-a-drawer-is-counted-by-denomination.md)

Accepted by the owner on 2026-11-03 ("ok hết, làm luôn").

## Context

A restaurant changes cashiers at midday on the same till. ADR-0167 says that "a handover is a close
and a new start", and the till offers only that, as two separate acts:

- The incoming cashier types the new float by hand. Nothing carries the outgoing count over.
- The incoming cashier never counts what they take on.
- No slip tells either cashier what changed hands.

Until the second open, the till also refuses cash or takes it into no shift.

A manager who wants to know how a shift stands has to open the console. The till has no X read.
ADR-0165 deferred it to D10, which asks whether an X read resets anything.

## Options considered

1. **Keep two acts, and pre-fill the float.** This is cheap, but nobody checks the cash that changes
   hands.
2. **One handover flow, with an incoming count, on the existing events** (chosen).
3. **A new "handover" state on the shift machine**, where one drawer session spans two cashiers. This
   blurs whose cash a variance is, and changes the state machine and the shift identity for no gain
   over two linked shifts.

## Decision

1. **The handover is one flow on the Shift screen, built from the acts that exist.**
   - **Step 1, outgoing cashier:** counts blind (ADR-0186's counter), then closes, with a reason when
     the variance is over the limit.
   - **Step 2, incoming cashier:** signs in, counts what is in the drawer, and opens. The float is the
     incoming count.
   - Both steps run on the same till, one after the other. The till does not leave the flow between
     them, so no sale falls between the two shifts.
2. **The link is recorded, additively.**
   - `cash.shift.opened` gains an optional `handed_over_from_shift_id`, the shift just closed on that
     drawer.
   - The difference between the outgoing count and the incoming count is the handover difference.
     The console reads it from the two events; no event carries it.
   - A non-zero difference asks the incoming cashier to recount once, then to confirm.
3. **A handover slip.**
   - Printed at the end of the flow, and reprintable from the drawer list.
   - It shows the till, the time, the outgoing count with its breakdown, the incoming count with its
     breakdown, and the difference.
   - It names no person, as the void slip names none (ADR-0184, Amendment). The shifts' own records
     say who acted, where ADR-0167 decision 9 allows it.
4. **An X read on the till, which resets nothing.**
   - A new edge read, `GET /api/shifts/{shift_id}/x`, and a store-wide `GET /api/shifts:x` across
     every open drawer.
   - Each answers:
     - sales and bill count;
     - totals by tender, of cash, card, QR and other;
     - discounts, and voids after payment;
     - paid in, paid out, and dropped to safe;
     - expected cash.
   - In a blind-close store it leaves expected cash out unless the reader holds the new permission.
   - **Permission:** a new `reports.shift.read_x`, granted by default to Manager and Owner, with no
     PIN. Reading is not an act on money.
   - **Printable:** printed as "X — not a close" in the paper's language.
   - **Additive:** both routes are additive under ADR-0111. The route snapshot gains lines and loses
     none.
   - **D10, the X half:** an X is a non-resetting read of the live shift. The Z remains the shift
     close and its printed report. Whether a Z is a one-time immutable artefact stays open.
5. **Store end of day on the till.**
   - When every drawer of the business date is closed, the drawer list says "All tills closed for the
     day".
   - It offers the store-wide summary: the last X across all drawers, which now reads closed shifts.
   - It is printable.

## Consequences accepted

- **Additions:** one optional field on `cash.shift.opened`, two edge read routes, and one permission.
  Snapshots gain lines and lose none.
- **Blind incoming count:** the incoming count is a person's figure about cash they did not count
  out. Variance by person stays where ADR-0167 decision 9 put it: only under `DRAWER_ACCESS_ASSIGNED`,
  and only on the tenant's stated legal basis. Reports show handover differences per drawer and per
  store, never ranked by person (as ADR-0184's second Amendment does for voids).
- **One till during the flow:** a cashier mid-handover holds the till for the two counts, about a
  minute. Other tills keep selling.
