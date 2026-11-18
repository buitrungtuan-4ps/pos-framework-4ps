# ADR-0218 — A printed check is an event every till sees

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Completes roadmap-v3 B2.1 and D9 · Needed by [ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md)
decision 5 · Relates to [ADR-0128](0128-a-bill-splits-and-merges.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)

Waits on the owner: the event's name, and whether a store may require a check before payment.

## Context

The check (pre-bill, *phiếu tạm tính*) prints today: `POST /api/orders/{id}/check/print`,
`/api/tables/{id}/check/print` and `/api/bills/{id}/check/print`, the act `billing.check.print`. It
opens no bill and takes no receipt number. Nothing records it: no event, no marker, no flag
(roadmap-v3 B2.1 left them for an owner-reviewed change). So another till cannot see that a table
has been shown its amount, a reprint leaves no trace, and an order changed after its check was
shown looks the same as one that was not. A full-service template needs all three.

## Options considered

1. **A new bill state, "presented".** D9 refused it: every bill act would gain a state to check, and
   a check is a piece of paper, not a commitment.
2. **Keep it an effect only.** Cheapest, but blind: the reprint and the stale check stay invisible.
3. **An event and a projection marker, no new state** (chosen; D9).

## Decision

1. **A new event, `billing.check.printed`** (version 1, additive): `order_id`, `bill_id` when the
   check is one part of a split bill, `total_due` as printed, `copy` (1 for the first check of that
   order or part, counting up), and no receipt number and nothing personal. It is written in its
   own transaction before the job goes to the printer (ADR-0196 safeguard 1); a printer that fails
   afterwards leaves the event standing, as a print job's failure is the print queue's to report.
2. **A projection marker, not a state.** The edge keeps, per order and per open part, the time and
   amount of the last check. The floor, the order screen and `GET /api/orders/live` show it on every
   till. A change to the order after it (a line added, voided or re-quantified, a reduction, a waived
   fee, a split or a merge) marks the check **stale** until the next print. Nothing is refused
   because of the marker.
3. **No capability flag.** Whether a store prints checks is the act's permission and whether the
   package puts the button on a screen. Roadmap-v3's `pre_bill` flag is not added.
4. **Reports** count checks and copies per shift, so a check reprinted many times is visible.

## Consequences accepted

- One new event in `pos-proto` and `docs/snapshots/events.txt`; a cloud reader that does not know it
  ignores it. The `PROTOCOL_VERSION` stays (an added event is additive).
- An event per print: a busy floor adds a few hundred small events a day.
- Requiring a check before payment, if the owner wants it, is a later setting and its own change.
