---
kind: changed
upgrade_note: "The bill, order and table check reads gain an optional `payments` list (each payment's `method`, `amount`, `taken_time` and optional `split_ways`; no person), and `GET /api/menu` gains `part_payments`. The pay screen's even split now posts each share but the last to `POST /api/bills/{id}/payments`, so a store with `billing.part_payments` off is offered no even split, and a till reading an edge without `part_payments` offers none. The share plan kept in the browser tab's session storage is gone, and a share taken can no longer be undone on the till: voiding the bill gives each payment back."
---
- **An even split's shares are recorded as they are taken.** Each guest's share is a payment on the
  edge the moment it is taken, so a reload, another tab or another till shows what is paid, each
  payment with its tender and time, and the share left; the last share settles the bill (ADR-0220).
  A store that takes a bill in one payment is not offered the even split.
