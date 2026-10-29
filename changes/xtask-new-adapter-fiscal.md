---
kind: added
---
- **`cargo xtask new adapter --kind fiscal` writes `crates/adapters/fiscal-sandbox`** (ADR-0176).
  It contains:
  - a `Fiscalization` over the shared offline allocator, so it issues with no network, gives one
    number per bill and never reuses one, and submits nowhere;
  - the `einvoice.sandbox` provider descriptor;
  - `tests/contract.rs`, which runs the shared `Fiscalization` suite, and a test that two ranges
    never share a number.

  The descriptor is registered nowhere until ADR-0156 settles how the cloud submits e-invoices.
  The generator prints that, and the console label to add, as next steps.
