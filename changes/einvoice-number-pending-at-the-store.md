---
kind: added
---
- **A store can issue its e-invoice "number pending" for the vendor to number** (ADR-0249,
  ADR-0251). Under the new `einvoice.number_source` value `EINVOICE_NUMBER_SOURCE_VENDOR` the settle
  (or the deferred issue) still records `fiscal.invoice.issued` at its moment, with the additive
  field `number_source` and `invoice_sequence` 0, and takes no number from the store's symbol. The
  settle answers `invoice_number_pending: true` with the lookup code; the till's paid panel says the
  number is pending; the receipt and its copies print `Số HĐĐT: Chờ cấp số`. A store switched back
  to the POS keeps issuing "number pending" until the vendor has numbered every waiting invoice.
  `EINVOICE_NUMBER_SOURCE_POS` stays the default and nothing changes for it. The console does not
  offer the setting yet.

  **Upgrade note (protocol):** `fiscal.invoice.issued` gains `invoice.number_source` (absent reads as
  `POS`), the `einvoice` node gains `number_source`, and the edge's bill answer gains
  `invoice_number_pending`. All are additive.
