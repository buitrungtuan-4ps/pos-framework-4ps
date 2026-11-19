---
kind: added
upgrade_note: "A new edge route, `POST /api/orders/{id}/weigh`, and act, `sales.line.weigh`, add a weighed item's line from the scale (`WEIGHT_SOURCE_SCALE`) or a typed weight (`WEIGHT_SOURCE_MANUAL`, only where `weighing.manual_entry` is on and the edge's country pack does not forbid it; `vn`, `jp` and `in` forbid it); `GET /api/scale` reads the scale. `GET /api/menu` gains `weighing_manual_entry` and `weighing_min_grams`. New refusals: `MANUAL_WEIGHT_NOT_ALLOWED`, `SCALE_NOT_SETTLED` and `SCALE_ABSENT`. No real scale driver ships yet: an edge without one answers `SCALE_ABSENT`."
---
- **A weighed item is weighed on the scale beside the till** (ADR-0223). The edge reads the scale's
  settled weight, takes the item's tare off and prices the net grams per kilogram; a moving or
  overloaded scale is refused, never read as a weight. Where the store allows it, a weight may be
  typed by hand instead, and each line records where its weight came from.
