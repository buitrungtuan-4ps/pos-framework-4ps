---
kind: fixed
---
- **A configuration preview no longer shows prices to Ops.** Both publish previews
  (`POST /admin/config/preview` and `POST /admin/config/campaigns/preview`) need only
  `console.config.publish`, which Ops holds. They removed the staff from their diff but not the
  priced nodes, so an Ops account could read a store's price book, its promotions' terms and its
  fees, which the configuration read already withholds from Ops (production-readiness S8). A
  preview's diff now leaves out `menu`, `campaigns` and `fees` for a role without
  `console.reports.revenue`, exactly as the read does. Owner and Admin see them as before. Whether
  anything would change is still worked out before the redaction, so a preview that changes only a
  price shows Ops an empty diff with `unchanged: false`. No role or permission changes.
