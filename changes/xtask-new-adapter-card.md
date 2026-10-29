---
kind: added
---
- **`cargo xtask new adapter --kind card` writes `crates/adapters/card-sandbox`** (ADR-0176). It
  contains:
  - a `PaymentTerminal` over a transport seam, whose ambiguous answer is `Unknown` with its
    reference, never a decline, and which refuses to void a settled capture;
  - the `card.sandbox` provider descriptor;
  - `tests/contract.rs`, which runs the shared `PaymentTerminal` suite.

  The descriptor is registered nowhere: a card driver runs on the edge, and registering it there
  waits for ADR-0154. The generator prints that, and the console label to add, as next steps.
