---
kind: added
upgrade_note: Migration 0106 adds the `einvoice_submissions` and `einvoice_feed_cursors` tables (additive). A new background task reads every active store's event log once for `fiscal.invoice.issued`; on a cloud with a long history its first pass reads each store's log from the start, 10,000 events per store per pass. A new alert kind, `einvoice_overdue`, is Critical.
---
- **The cloud submits the e-invoices stores issue, and warns before the deadline** (ADR-0241). A
  background task records each `fiscal.invoice.issued` once and submits it through the sandbox
  provider, retrying with a doubling wait and stopping on a refusal; each invoice's state is kept in
  PostgreSQL. The `einvoice_overdue` alert fires for a store while any of its invoices is not with
  the provider six hours before its deadline (the end of the next day; to be confirmed by legal).
  `POS_DEMO_EINVOICE=refuse` or `=timeout` scripts the sandbox for a demo.
