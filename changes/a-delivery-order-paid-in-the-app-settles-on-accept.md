---
kind: added
upgrade_note: A delivery connection's new `payment` defaults to `VENDOR_PAYMENT_IN_APP`, so once a store runs this release, each delivery order it takes is settled "Paid via <vendor>" (`PAYMENT_METHOD_VENDOR`, a new payment method) when it is accepted, and no longer waits at the till. Set `payment` to `VENDOR_PAYMENT_AT_STORE` on a connection whose guests pay at the store. Readers of `billing.payment.captured` meet the new method token; the till's shift read counts it under other tenders.
---
- **A delivery order paid in the app settles when the store accepts it.** Each delivery connection
  says where its guests pay (`payment`: in the app by default, or at the store). In the app, the
  edge settles the order's bill with the new tender `PAYMENT_METHOD_VENDOR` as it takes the order,
  and its e-invoice is issued for the bill as any other; the console's X/Z report shows it as paid
  via delivery apps (ADR-0248 decisions 2 and 6).
