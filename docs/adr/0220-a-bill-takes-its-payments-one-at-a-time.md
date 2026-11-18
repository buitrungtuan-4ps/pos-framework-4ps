# ADR-0220 — A bill takes its payments one at a time, and every till sees what is paid

**Status** Accepted 2026-11-18 (owner) · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Completes roadmap-v3 B2.4 · Needed by [ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md)
decision 5 · Amends [ADR-0028](0028-settlement-and-payment-invariant.md) (when payments are
recorded) · Relates to [ADR-0128](0128-a-bill-splits-and-merges.md), [ADR-0184](0184-a-settled-bill-is-voided-whole-not-refunded.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)

Accepted by the owner on 2026-11-18 as recommended: once a payment is taken, the bill is neither
split nor merged and a reduction may take it no lower than what is paid; a shift does not close while
one of its drawer's bills is part paid; one bill has one receipt and one number, taken at the last
payment.

## Context

A bill splits by lines (ADR-0128). An **even** split ("four ways") is held on the till: the shares
already taken live in the browser tab's storage (`ui/src/state/split-plan.ts`) until the last
share lands, and the edge settles all tenders in one step (`BillCommand::Settle`). Another till sees
the whole amount owing, a lost tab forgets the money taken, and the drawer holds cash no event
explains until the bill settles. Recording a payment earlier changes when money is recorded and
how a bill settles (AGENTS.md §7).

## Options considered

1. **Keep the plan on the till, synchronised to other tills.** Money would live outside the log.
2. **Split the bill into N bills of equal amounts.** A bill covers lines, not amounts (ADR-0128).
3. **The edge records each payment as it is taken; the last one settles** (chosen).

## Decision

1. **`POST /api/bills/{id}/payments`** takes one tender (method, tendered, applied, tip) on an open
   bill and records `billing.payment.captured` at once, in its own transaction. The bill stays
   open. The cash drawer opens after the commit, as on settle.
2. **The last payment settles.** When the applied payments reach `total_due`, the same transaction
   records `billing.bill.settled`, taking the receipt number then. One bill, one receipt, one number,
   listing every tender. `POST /api/bills/{id}/settle` keeps working and counts payments already
   taken. A payment above what remains is refused, never turned into change on another tender.
3. **Every till sees it.** The bill and check reads carry `paid_total`, `remaining` and the payments
   (method, amount, time; no person); the fan-out tells other tills. An even split's share count is
   an additive optional field, `split_ways` (2 to 20), on `billing.payment.captured`. The till's
   session-storage plan is retired.
4. **Once a payment is taken** the bill cannot be split or merged, and a reduction, comp or waived
   fee may not bring `total_due` below `paid_total`. Voiding a part-paid bill needs the void's
   permission and PIN and gives back each captured payment, as ADR-0184 decision 6 does for a
   settled bill.
5. **Shift.** A payment counts in the shift of the drawer it was taken into. Closing a shift is
   refused while a bill holds a payment taken into its drawer and has not settled.
6. **Catalogue.** A new act, `billing.payment.take` (`ACTION_EFFECT_MONEY`), with the settle's
   predictable refusals.

## Configuration

- **A setting** (ADR-0160): `billing.part_payments`, on or off, **default on**: whether the till
  offers taking a bill in parts. The full-service, bar and self-order templates keep it on; the café,
  fast-food and bakery-retail templates recommend off, since a counter bill is paid at once.
- **How many even shares** a bill may be split into stays `tender_keys.split_ways_max`.
- **Which tenders** a store takes stays its tender configuration (roadmap-v3 B4.5).
- **Fixed rules,** all money integrity: a payment is an event when it is taken; no split or merge
  after a payment; no reduction below what is paid; no shift close over a part-paid bill of its
  drawer; one receipt number per bill, at the last payment (ADR-0025); a voided part-paid bill gives
  each payment back.

## Consequences accepted

- `billing.payment.captured` may now precede its settle by minutes; a reader that assumed they
  arrive together (shift roll-ups, the cloud's reports) is checked and fixed in the same work.
- One additive event field, one route and one act; ADR-0028's invariant holds at settle unchanged.
- The e-invoice remains one per bill at settle.
