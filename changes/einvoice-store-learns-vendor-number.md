---
kind: added
---
- **A store learns the vendor's number for an invoice it issued "number pending", and the console
  offers `einvoice.number_source`** (ADR-0251 decisions 4, 5 and 8). Each minute the edge records
  `fiscal.invoice.numbered` once for every number the cloud wrote on its `einvoice` node for a bill
  still waiting; a copy of the receipt then prints the number, and the store's own sequence moves
  past it, so a store switched back to the POS never numbers over it. The console's settings offer
  **Who numbers e-invoices**: the store (`EINVOICE_NUMBER_SOURCE_POS`, the default, works offline)
  or the vendor (`_VENDOR`). The E-invoices screen shows a waiting invoice as "Number pending" and
  offers no correction until it is numbered; the correction route refuses it
  `409 E_INVOICE_NUMBER_PENDING`. Whether a seller may let the vendor number is to be confirmed by
  legal and the vendor.

  **Upgrade note (protocol, setting):** the new event `fiscal.invoice.numbered` is additive. The new
  setting `einvoice.number_source` defaults to `EINVOICE_NUMBER_SOURCE_POS`, so nothing changes
  until a tenant or store chooses the vendor.
