---
kind: added
upgrade_note: Migration 0108 adds an index on an e-invoice submission's bill (additive). A new setting, `einvoice.correction_kind`, defaults to `EINVOICE_KIND_ADJUSTMENT`; the till still refuses to void an invoiced bill until a later release offers the other routes (ADR-0245).
---
- **The cloud corrects the invoice of a bill voided at the store** (ADR-0245). When a store's log
  carries `billing.bill.voided` for a bill whose invoice was issued, the e-invoice feed records the
  correction under the tenant's correction symbol: an `ADJUSTMENT` back to zero, or a `REPLACEMENT`
  of zero where `einvoice.correction_kind` says so. A void read twice is corrected once, and no
  invoice goes below zero. The console's correction and the feed number corrections through one
  path. Which errors need which kind is to be confirmed by legal or the accountant.
