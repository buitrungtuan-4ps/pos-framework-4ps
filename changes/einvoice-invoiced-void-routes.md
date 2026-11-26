---
kind: added
upgrade_note: Three new settings, `einvoice.invoiced_correction` (default `INVOICED_CORRECTION_HQ_ONLY`, today's behaviour), `einvoice.manager_correction_limit` (default `0`) and `einvoice.cancel_window_minutes` (default `0`). Nothing changes until a tenant or store chooses another route.
---
- **A store chooses how a bill with an issued e-invoice is voided** (ADR-0245). Under
  `INVOICED_CORRECTION_MANAGER_UP_TO_LIMIT` a manager voids it at the till when its total is within
  `einvoice.manager_correction_limit`; under `INVOICED_CORRECTION_CANCEL_WINDOW` while its invoice
  is younger than `einvoice.cancel_window_minutes`. The cloud then corrects the invoice. The
  default, `INVOICED_CORRECTION_HQ_ONLY`, still refuses at the till. Every legal point is to be
  confirmed by legal or the accountant.
