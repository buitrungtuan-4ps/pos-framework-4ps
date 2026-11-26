---
kind: added
upgrade_note: Migration 0109 adds the table `einvoice_correction_requests` (additive). Three console routes are added under `/admin/einvoices/correction-requests`; the decide routes need `console.einvoices.correct` (OWNER and ADMIN).
---
- **HQ's queue of correction requests** (ADR-0250). The cloud's e-invoice feed records each
  `fiscal.correction.requested` a store sends once, as `PENDING`, with the requester's id. HQ lists
  the queue and approves or declines a request; the decision records the approver's id, is written
  onto the store's own `einvoice` node as `correction_decisions` and published, and is audited with
  the requester and the approver. Nothing is issued by a decision: the store's approved void is what
  the cloud corrects. Whether a void HQ approved may happen after the store's void window is to be
  confirmed by legal or the accountant.
