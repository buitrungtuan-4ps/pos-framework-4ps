---
kind: fixed
upgrade_note: The total an inbound order (QR, delivery, the public API) is answered with now includes tax and fees, as the order's bill will charge. An integrator that added tax to it itself must stop doing so.
---
- **An order from outside the store is answered with what its bill will charge** (ADR-0248 decision
  4, phase 10 open item O6). The acceptance's `total`, which a delivery app or the public API
  receives and the idempotency ledger keeps for a retry, is now the bill's total with tax and fees,
  not the lines' sum before tax (217,000 ₫ answered for an order charged 236,000 ₫). The till's
  guest-order queue shows the same figure.
