# ADR-0257 — A void HQ approved carries the kind HQ approved

**Status** Accepted 2026-11-27 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-27 · Phase 11 (O2, "data matched from edge to console") · Supersedes
[ADR-0256](0256-a-voided-bills-invoice-is-corrected-by-the-kind-the-store-applied.md) decision 2
in part · Relates to [ADR-0250](0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)

Which errors need an adjustment and which a replacement is still **to be confirmed by legal or the
accountant**.

## Context

ADR-0256 decision 2 has the edge write, on the void of an invoiced bill, the `einvoice.correction_kind`
of the node it runs. A void the store may make only because HQ approved a correction request
(`REQUEST`, or `MANAGER_UP_TO_LIMIT` above the limit, or `CANCEL_WINDOW` after the window) can come
hours after the request. If the kind changes in between, the void carries the new kind, while the
request the store sent (`fiscal.correction.requested`), the console queue and HQ's approval all said
the old one. The real edge-to-cloud run behind ADR-0256 had exactly this case: a 240,000 ₫ bill was
requested and approved as an adjustment, and the kind was changed to replacement before the till voided.

## Options considered

1. **The cloud prefers the approved request's kind over the void's.** The void event would then name
   a kind the console does not show, and the edge's record and the console's would disagree.
2. **The edge writes the kind of its own request when the void rides HQ's approval.** The store
   already holds the request in its log; the event says what was approved and applied.

## Decision

1. When the edge voids an invoiced bill **because HQ approved** its correction request (the route
   alone would refuse it), the void's `correction_kind` is the kind of the store's own latest
   `fiscal.correction.requested` for that bill. Otherwise, as ADR-0256 decision 2: the node's
   `einvoice.correction_kind`.
2. The edge keeps, per bill, the kind of its latest correction request, folded from its log, so a
   restart keeps it. It is bounded as the store's correction decisions are (newest 200 bills).
3. The cloud is unchanged (ADR-0256 decision 3).

## Consequences accepted

- No event or setting changes; the field ADR-0256 added carries the approved kind in this case.
- A kind changed after HQ's approval does not reach that bill; the console's correction is the one
  HQ approved.
