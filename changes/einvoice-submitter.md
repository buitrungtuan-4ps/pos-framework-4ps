---
kind: added
---
- **The cloud's e-invoice submission queue** (ADR-0225 decision 6), in
  `pos_cloud::einvoice::submitter`. The queue holds up to 10,000 invoices and is idempotent by
  bill. A provider that cannot be reached is retried after one minute, and the wait doubles up to
  an hour. A provider that refuses an invoice stops it, and it is corrected in the console rather
  than resubmitted. An invoice not accepted within the deadline is reported as overdue; the
  deadline is the country's and is still to be confirmed. All of this is proved against
  `einvoice.sandbox`. It is not yet fed from the store's `fiscal.invoice.issued` events or kept in
  the database; that wiring is the next slice.
