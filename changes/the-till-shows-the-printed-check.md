---
kind: added
upgrade_note: "`GET /api/menu` gains `check_before_payment`, the store's `billing.check_before_payment` (false unless the store turns it on). A till older than the field ignores it, and the edge still refuses a payment the setting forbids."
---
- **The till shows the printed check.** The order and pay screens say when the check (pre-bill)
  was printed and which copy it was, and that it is out of date once the order changes. A store
  that takes payment only against a fresh check has the pay pad refuse a payment without one
  before it is sent (ADR-0218).
