---
kind: added
upgrade_note: Additive events `fiscal.correction.requested` and `fiscal.correction.decided`, an additive `correction_decisions` field on the `einvoice` node, a new setting `einvoice.correction_reason_required` (default on) and a new refusal token `CORRECTION_NOT_NEEDED`. Nothing changes for a store until the console offers `INVOICED_CORRECTION_REQUEST` and HQ decides a request.
---
- **A store manager can ask HQ to correct a bill whose e-invoice was issued** (ADR-0245 route b,
  ADR-0250). `POST /api/bills/{id}/correction_request` records the request, with a reason unless
  `einvoice.correction_reason_required` is off, under `billing.bill.void_settled` and its PIN, and
  moves no money. Once the store's `einvoice` node carries HQ's approval for the bill, the till voids
  it once, even after the store's void window. The console queue and the till form follow; until
  then `INVOICED_CORRECTION_REQUEST` is not offered.
