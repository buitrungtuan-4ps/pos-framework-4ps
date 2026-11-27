---
kind: added
upgrade_note: The revenue rollup, and the revenue and X/Z reports that serve it, add `by_channel` and `by_app_tender` to each day; no field changes. A day folded before the upgrade has neither until the store's rollup is reset (`POST /admin/stores/{id}/rollups/reset`), which re-folds the log.
---
- **The cloud reports each day's sales by channel and order type (ADR-0259).** One line per channel
  (the store's POS, QR, API, or a delivery app by its connection), app and order type (dine-in,
  take-away, delivery), with its bills and sales, summing to the day's; and each delivery app's
  in-app payments apart. A bill voided after payment leaves both.
