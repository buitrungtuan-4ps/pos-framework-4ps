---
kind: added
---
- **The cloud asks the e-invoice provider to number an invoice a store issued "number pending"**
  (ADR-0251 decisions 3 and 4). The feed records it under the placeholder `PENDING/<invoice id>`,
  submits it with no number, and replaces the placeholder with the provider's number when it
  answers. The cloud then writes the number into `vendor_numbers` on the store's own `einvoice` node
  (Store layer, newest 200, not a setting) and publishes it, for the store to learn. A void of such a
  bill is corrected once the original has its number. Nothing changes for an invoice the store
  numbered itself.

  **Upgrade note (protocol):** the `einvoice` node gains `vendor_numbers`, additive and written only
  by the cloud. The submissions table's outcome write now also writes the row's number, which only a
  pending invoice changes; no migration.
