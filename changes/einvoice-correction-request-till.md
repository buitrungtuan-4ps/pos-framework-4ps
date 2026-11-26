---
kind: added
upgrade_note: "`einvoice.invoiced_correction` now offers `INVOICED_CORRECTION_REQUEST` (a manager asks HQ from the till; HQ approves in the console's queue). The default stays `INVOICED_CORRECTION_HQ_ONLY`. The till's message for `E_INVOICE_ISSUED` now says HQ corrects the bill rather than that the invoice must be cancelled."
---
- **A manager asks HQ to correct an invoiced bill from the till** (ADR-0245 route b, ADR-0250).
  Where a paid bill's void is refused because its e-invoice was issued, the void panel offers
  **Ask HQ to correct**, sent with the reason and the manager's code and PIN the void was tried with.
  Nothing is voided and no money moves; once HQ approves in the console's queue, the manager voids
  the bill on the same panel, once. A request HQ does not need is refused with
  `CORRECTION_NOT_NEEDED`, in English and Vietnamese. The console now offers
  `INVOICED_CORRECTION_REQUEST` for `einvoice.invoiced_correction`. Whether a void HQ approved may
  happen after the store's void window is to be confirmed by legal or the accountant.
