# ADR-0256 — A voided bill's invoice is corrected by the kind the store applied

**Status** Accepted 2026-11-27 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-27 · Phase 11 (O2, "data matched from edge to console") · Adds a field to
`billing.bill.voided` (`AGENTS.md` §7) · Details
[ADR-0245](0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md) decision 2 and
[ADR-0250](0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)

Which errors need an adjustment and which a replacement is still **to be confirmed by legal or the
accountant**. This record changes only *whose* choice of kind the cloud applies, not the choices.

## Context

A real store and a real cloud were run together (phase 11, Agent AB). The cloud corrects the invoice of
a bill voided at the till with the `einvoice.correction_kind` it reads **when its feed reaches the
void**, not the one the store ran when it voided. Two bills in that run show the gap:

- under `MANAGER_UP_TO_LIMIT` with `EINVOICE_KIND_ADJUSTMENT`, the store voided a 150,000 ₫ bill; the
  kind was then changed to `REPLACEMENT` before the feed's next pass, and the console shows a
  replacement;
- a manager asked HQ to correct a 240,000 ₫ bill. The request said `ADJUSTMENT` (from
  `fiscal.correction.requested`), the console queue showed "Adjustment" and HQ approved it. The kind
  then changed, the till voided, and the console shows a replacement.

The gap is wider for a store that is offline: its voids reach the cloud hours later, under whatever
the setting is by then. `billing.bill.voided` carries only the bill and the reason, so the cloud cannot
tell which kind the store applied.

## Options considered

1. **The cloud reads the store's configuration version current at the void's time.** No event change,
   but it guesses: a store applies a version some seconds after it is published, and later when
   offline.
2. **The cloud uses the approved request's kind, and the current setting otherwise.** Fixes the request
   case only; a void under the manager's limit or inside the cancel window still drifts.
3. **The void says which kind the store applied.** One optional field on the event, written by the
   only party that knows.

## Decision

1. `billing.bill.voided` gains an optional `correction_kind` (`EINVOICE_KIND_ADJUSTMENT` or
   `EINVOICE_KIND_REPLACEMENT`). Additive: left off the wire when absent, absent on every void written
   before it, and on a void of a bill with no issued e-invoice. Schema version stays 1.
2. The edge writes it when it voids a bill with an issued e-invoice: the `einvoice.correction_kind` of
   the node it ran the void under.
3. The cloud corrects the voided bill's invoice with, in order: the kind the void carries; else the
   kind of the bill's **approved** correction request (what HQ approved); else the current setting
   (a void from an edge older than this record).
4. Nothing else changes: numbering under the correction symbol, never below zero, one correction per
   void event.

## Consequences accepted

- One field on a published event, with its snapshot line; older clouds ignore it.
- The console's correction now always shows the kind the till applied, so edge and console agree.
- A kind changed after a void no longer reaches that void. Correcting it is HQ's console correction,
  as for any invoice.
