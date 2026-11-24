---
kind: added
upgrade_note: The `Fiscalization` port's `InvoiceRequest` gains two fields, `issued_number` and `correction` (ADR-0225 decision 8). An adapter outside this tree must set both, `None` to keep its behaviour, and must pass the two new contract cases.
---
- **An e-invoice sandbox provider, and a port that submits the store's own number** (ADR-0225).
  The cloud's provider catalogue lists `einvoice.sandbox`, which accepts, refuses or times out on
  command, so e-invoice submission can be driven without a vendor. `Fiscalization` now carries the
  number the store issued and what a correction corrects, and two contract cases hold every adapter
  to it: the store's number is submitted unchanged, and a number another bill already holds is
  refused.
