---
kind: added
---
- **`cargo xtask new adapter --kind erp` writes `crates/adapters/erp-sandbox`** (ADR-0176). It
  contains:
  - an `ErpSink` over a transport seam. The seam is handed each batch's idempotency key, so a
    day posts once per revision, a later revision supersedes, an unknown account posts nothing,
    and a closed period is refused, not retried. Amounts stay integer minor units;
  - the `erp.sandbox` provider descriptor;
  - `tests/contract.rs`, which runs the shared `ErpSink` suite and a closed-period test.

  The generator registers the descriptor nowhere. It prints where registering it would go and the
  console label to add.
