---
kind: added
upgrade_note: Migration 0087 runs once, behind its data_migrations marker - every existing role gets billing.bill.void_settled directly where it grants billing.bill.void directly and with approval otherwise, and every store whose people were published is queued to be published again. The new setting billing.settled_void_window defaults to SETTLED_VOID_WINDOW_BUSINESS_DATE, and nothing at a store changes until the till can void a paid bill.
---
- **A paid bill can be voided whole, by its own permission, inside a window the store sets**
  ([ADR-0184](../docs/adr/0184-a-settled-bill-is-voided-whole-not-refunded.md)), in the domain and
  the catalogue; the till's route follows. The bill machine gains `SETTLED → VOIDED` by its own
  trigger, `void_settled`, so `SETTLED` is no longer terminal and `VOIDED` still is. The decision
  refuses a bill outside the store's window, one with an issued legal invoice, and anyone without
  the new permission `billing.bill.void_settled` (PIN-flagged, Manager and Owner by default) or
  its PIN, and prints a void slip. The window is the new setting `billing.settled_void_window`:
  `SETTLED_VOID_WINDOW_BUSINESS_DATE` (the bill's own business date, the default) or
  `SETTLED_VOID_WINDOW_SHIFT` (while the shift that took the money is open).
