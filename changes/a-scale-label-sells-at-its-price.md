---
kind: added
upgrade_note: "A new edge route, `POST /api/orders/{id}/scan`, and act, `sales.line.scan`, add the line a scanned code names. A new setting, `scan.variable_measure_layout` (`VARIABLE_MEASURE_LAYOUT_NONE` by default, `PRICE_5` or `WEIGHT_5`), reads EAN-13 codes with GS1 prefix 20 to 29; the `zz` pack seeds `PRICE_5` for a new store, and `vn`, `jp` and `in` seed nothing until their GS1 practice is confirmed. New refusals: `UNKNOWN_CODE`, `PRICE_LABEL_NOT_OPEN_PRICE` and `WEIGHT_LABEL_NOT_SOLD`."
---
- **A scale's price label rings up at its price** (ADR-0222). The edge reads a variable-measure
  code by the store's layout and sells the open-price item its PLU names at the label's price,
  within the item's bounds; a fixed-price item refuses a price label, and a weight label waits for
  items sold by weight. A barcode or PLU scanned through the same route sells one at the menu's
  price.
