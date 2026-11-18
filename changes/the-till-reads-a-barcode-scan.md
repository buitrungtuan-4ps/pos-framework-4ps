---
kind: added
upgrade_note: "A new settings node, `scan`, holds `scan.keystroke_window_ms` (20 to 200, default 50), with a new additive setting unit `SETTING_UNIT_MILLISECONDS`. `GET /api/menu` gains `scan_keystroke_window_ms` and `barcode_enabled`. The bakery-retail template's demo items carry fake barcodes and PLUs."
---
- **The till reads a barcode scan.** Where a store sells by barcode, the order screen listens for a
  keyboard-wedge scanner: digits arriving within the store's scan window and ending in Enter are
  looked up in the till's index of the menu's barcodes and PLUs, and the item is added as if its
  button were tapped; a code the menu does not carry says so (ADR-0222).
