---
kind: fixed
upgrade_note: `billing.bill.voided` gains an optional `correction_kind` (additive, schema version 1, ADR-0256). An edge on this release writes it on each void of a bill with an issued e-invoice; a cloud on this release corrects with it, and for a void without it uses the kind of the bill's approved correction request, then the current `einvoice.correction_kind`. Upgrade the cloud first or together; an older cloud ignores the field.
---
- **A voided bill's e-invoice is corrected by the kind the store applied.** The cloud used to read
  `einvoice.correction_kind` when it reached the void, so a kind changed in between, or a store
  offline for a while, gave the console a replacement for a bill the store voided as an adjustment,
  even where HQ had approved an adjustment. The void now says which kind the till ran, and the
  console's correction follows it (ADR-0256).
