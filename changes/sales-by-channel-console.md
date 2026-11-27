---
kind: added
upgrade_note: The X/Z report (`GET /admin/stores/{store_id}/reports/xz`) adds `vendor_names`; no field changes.
---
- **The console's X/Z shows sales by channel and order type, and names each delivery app
  (ADR-0259).** Each line reads where the orders came from and how they were served,
  "GrabFood · Delivery" ("GrabFood · Giao hàng"), with the same grouped by channel and by type;
  money paid in an app reads "Paid via GrabFood" ("Trả qua GrabFood") instead of one
  "Paid via delivery apps" row.
