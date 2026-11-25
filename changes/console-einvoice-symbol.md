---
kind: added
---
- **The console sets a store's e-invoice symbol.** A new **E-invoices** screen, under Compliance,
  shows the symbol the store numbers its e-invoices under, warns when a store that issues
  e-invoices has none (it cannot take payment until it has one), takes a new symbol by hand and
  publishes it through `PUT /admin/config/einvoice-symbol`, and shows the store's audit trail.
