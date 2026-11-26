---
kind: changed
---
- **The P17 load driver runs against a Vietnamese store's real edge** (`ui/perf/load-p17.mjs`). It
  opens each counter order at `CHANNEL`, or at take-away where the store refuses an order without a
  channel, so the edge prices and taxes each line at that channel, pays the check's total with tax,
  and reports the channel and how many e-invoices the settles issued, numbered or "number pending".
