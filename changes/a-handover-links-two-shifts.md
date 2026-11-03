---
kind: added
upgrade_note: `cash.shift.opened` gains an optional `handed_over_from_shift_id` (ADR-0187); it stays at `schema_version` 1 and `PROTOCOL_VERSION` stays 1. `POST /api/shifts` accepts it, refuses one that is not the shift just closed on that drawer with `409 NOT_THE_SHIFT_HANDED_OVER`, and answers `handover_difference` and `handover_slip_print`. The cloud's cash rollup gains `handover_difference` and `by_shift` (serde default); a handover opened before the rollup kept them reads as two shifts until the rollup is reset (ADR-0036). An older edge ignores the field, and an older till sends none.
---
- **A handover links the two shifts, and the console counts its cash once**
  ([ADR-0187](../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)). An opening
  may name the shift just closed on its drawer; the edge prints a handover slip with both counts and
  the difference, naming no person. The console's X/Z report no longer counts handed-over cash twice
  (a drawer handed over from A to B showed both floats and both expected amounts): the drawer
  expects what it holds and shows its handover difference, and a drawer whose shifts name no
  handover lists each shift on its own row rather than a sum. The till's handover flow comes
  separately.
