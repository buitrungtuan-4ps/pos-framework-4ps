---
kind: added
---
- **The console reads other cells' rollups.** `GET /admin/hq/rollups?from=&to=`, behind
  `console.data.read`, answers whether `[federation]` is configured and, per configured store in
  another cell, its daily rollups or the reason there are none (ADR-0179 C10). **Only aggregates
  cross a cell boundary**: a store's daily event counts, in total and by event type. No personal
  data, no `subject_id`, no per-person row and no raw event. No key is ever answered or logged. The
  whole read is bounded to 30 seconds; a store not reached in time answers so, and is not called.
  `pos_cloud` wires the `[federation]` section and the HTTPS fetcher at boot. No permission, schema
  or protocol change: the route is additive in the admin OpenAPI document.
