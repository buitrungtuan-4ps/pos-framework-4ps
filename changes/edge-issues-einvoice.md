---
kind: added
upgrade_note: This release adds two events, `fiscal.invoice.issued` and `fiscal.invoice.scheduled` (version 1, additive), and a new `einvoice` config node with three settings, `einvoice.issue_moment`, `einvoice.delay_minutes` and `einvoice.batch_minute`. `einvoice.issue_moment` defaults to `EINVOICE_ISSUE_MOMENT_OFF`, so nothing changes for an existing store. A new store's Vietnamese pack seeds `EINVOICE_ISSUE_MOMENT_IMMEDIATE`, and such a store refuses to settle (`409 E_INVOICE_SYMBOL_MISSING`) until the cloud publishes its invoice symbol. The cloud refuses an `einvoice.delay_minutes` past the country's legal limit (`SETTING_BEYOND_COUNTRY_LIMIT`).
---
- **A settle issues the store's e-invoice on its schedule** (ADR-0225, ADR-0229). The store can
  issue immediately, after a set delay, in a daily batch, only on request, or not at all. An issued
  invoice takes the next number of a gapless sequence under the store's symbol and is written as
  `fiscal.invoice.issued`. A deferred invoice waits as `fiscal.invoice.scheduled` and is numbered
  when the edge issues it, so a bill voided while waiting leaves no gap. The country's legal limit
  (for Vietnam, the end of the sale's day, still to be confirmed) is refused by the cloud and
  clamped by the edge. No personal data enters either event, and a paid bill with an invoice is
  corrected in the console instead of voided at the store (`E_INVOICE_ISSUED`).
