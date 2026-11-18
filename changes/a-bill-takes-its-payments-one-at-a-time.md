---
kind: added
upgrade_note: "A new edge route, `POST /api/bills/{id}/payments`, and act, `billing.payment.take`, record a payment as it is taken, so `billing.payment.captured` may now precede its bill's `billing.bill.settled` by minutes; the edge's own folds and the cloud's tender and void roll-ups already key a payment by its bill. `billing.payment.captured` gains an optional `split_ways`. A new setting, `billing.part_payments`, defaults to on; the café, fast-food and bakery-retail templates recommend off. New refusals: `PAYMENT_EXCEEDS_REMAINING`, `PART_PAYMENTS_OFF` and `BILL_PART_PAID`, the last for splitting, merging, voiding or reducing below what is paid a bill that holds a payment, and for closing the shift of the drawer it went into. The check reads gain optional `paid_total` and `remaining`."
---
- **A bill takes its payments one at a time.** Each payment towards a bill is recorded the moment
  it is taken and every till sees what is paid and what remains; the payment that covers the rest
  settles the bill, with one receipt (ADR-0220). A part-paid bill cannot be split, merged, voided
  or discounted below what is paid, and its drawer's shift waits for it to settle. A store can
  switch part payments off. The pay screen keeps its even split on the till until its own change.
