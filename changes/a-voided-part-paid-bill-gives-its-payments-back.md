---
kind: changed
upgrade_note: "Voiding a bill that holds payments taken one at a time is no longer refused with `BILL_PART_PAID`: it gives each payment back, its cash out of the voiding till's drawer (the void is stamped with that drawer's shift) and every other tender recorded as reversed. The answer of `POST /api/bills/{id}/void` gains optional `cash_returned`, `other_reversed` and `drawer_open`. The cloud's cash report counts the cash given back against the voiding till's drawer."
---
- **A part-paid bill can be voided.** Voiding a bill that guests have already paid part of gives
  each payment back: the cash comes out of the drawer, which opens for it, and card payments are
  marked reversed (ADR-0220). Until now such a bill had to be paid in full first.
