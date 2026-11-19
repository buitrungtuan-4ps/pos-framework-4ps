---
kind: added
upgrade_note: "The component catalogue gains `scan_field` and `plu_keypad`, which the default package's `order` screen now draws above the menu; a package version that leaves them out of its order screen no longer listens for a scanner there."
---
- **Quick sale: a code field, a PLU pad and typed open prices** (ADR-0222). Where the store sells by
  barcode, the order screen shows a code field, which takes a scan or a typed code, and a PLU pad.
  A code the till does not know goes to the store server on a counter order, which reads a scale's
  price label. An open-price item asks its price, within its published bounds, before it sells.
