---
kind: added
upgrade_note: New permission `reports.shift.read_x` (ADR-0187), default roles Manager and Owner, no PIN; the permissions snapshot gains a line. No migration grants it to roles authored before it, so an owner grants it in the console where an existing manager should see expected cash on a blind-close X. New edge routes, additive under ADR-0111: `GET /api/shifts/{id}/x`, `GET /api/shifts:x`, `POST /api/shifts:print_x` and `POST /api/shifts/{id}/handover-slip/print`. The cloud's revenue rollup gains `by_tender` (serde default).
---
- **The till reads a shift's X, and the store's**
  ([ADR-0187](../docs/adr/0187-a-shift-is-handed-over-at-the-till-and-read-there.md)). A read that
  resets nothing: sales, bills, cash/card/QR/other, discounts, voids after payment, paid in and out,
  dropped to safe, and the expected cash, which a blind-close store shows only to a holder of the new
  `reports.shift.read_x`. The store-wide X reads every open drawer, and once all are closed the
  day's shifts as its end of day. Both print as "X - not a close", and a handover slip can be printed
  again. The console's X/Z report shows a dash, not 0, for the expected drawer while no shift has
  closed, and splits the day by tender. The till's screens come separately.
