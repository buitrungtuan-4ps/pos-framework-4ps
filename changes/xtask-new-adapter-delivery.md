---
kind: added
---
- **`cargo xtask new adapter --kind delivery` writes `crates/adapters/delivery-sandbox`**
  (ADR-0176). It contains:
  - a `DeliveryVendor` over a transport seam, whose `decide` says what an accept, a reject or a
    ready may do to an order. A decision repeated for the same platform order is sent once. A
    late, out-of-order or contradicting one is `failed_precondition`, and an unknown order is
    `not_found`. No customer details cross it;
  - the `delivery.sandbox` provider descriptor;
  - `tests/contract.rs`, which runs the shared `DeliveryVendor` suite and the rules it does not
    reach.

  The generator registers the descriptor nowhere. It prints where registering it would go and the
  console label to add.
