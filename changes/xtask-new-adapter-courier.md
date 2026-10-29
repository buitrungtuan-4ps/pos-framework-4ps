---
kind: added
---
- **`cargo xtask new adapter --kind courier` writes `crates/adapters/courier-sandbox`** (ADR-0176).
  It contains:
  - a `ShippingDispatch` over a transport seam, idempotent by shipment, which refuses to cancel a
    completed job, cancels a cancelled one again without error, and still tracks a finished one;
  - the `courier.sandbox` provider descriptor;
  - `tests/contract.rs`, which runs the shared `ShippingDispatch` suite.

  The generator registers the descriptor nowhere. It prints where registering it would go (the
  cloud's provider registry) and the console label to add.
