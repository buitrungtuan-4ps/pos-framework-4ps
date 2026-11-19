---
kind: added
upgrade_note: "Additive: an item's `codes` may carry `sold_by` (`SOLD_BY_UNIT` by default, or `SOLD_BY_WEIGHT`) and `tare_grams`; `sales.order_line.added` gains an optional `weight_source` (`WEIGHT_SOURCE_LABEL`, `_SCALE`, `_MANUAL`); `GET /api/menu` carries `sold_by_weight` on each item. Two new settings on a new `weighing` node: `weighing.manual_entry` (off by default; the `zz` pack seeds it on, `vn`, `jp` and `in` seed nothing) and `weighing.min_grams` (1 to 1,000, default 1). New refusals: `WEIGHT_REQUIRED`, `NOT_SOLD_BY_WEIGHT`, `WEIGHT_BELOW_MINIMUM` and `WEIGHT_ABOVE_MAXIMUM`."
---
- **A scale's weight label rings up a weighed item** (ADR-0223). An item sold by weight is priced
  per kilogram and its line counts net grams, rounded once; a variable-measure weight label sells
  it by the label's grams and the line records the label as its weight's source. A weighed item is
  never sold by the piece, and a line under the store's least weight is refused.
