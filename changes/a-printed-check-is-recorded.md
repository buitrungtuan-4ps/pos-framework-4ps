---
kind: added
upgrade_note: A new event, `billing.check.printed`, is written for every check (pre-bill) a till prints; a cloud reader that does not know it ignores it. A new setting, `billing.check_before_payment`, defaults to off, which is what every store did before; a store that turns it on refuses payment for a bill without a fresh check (`CHECK_REQUIRED`). The check reads gain an optional `check_printed`.
---
- **A printed check is recorded and every till sees it.** Each check a till prints is now an event,
  and the order's check reads say when it printed, what it showed, which copy it was, and whether the
  order has changed since. A store can require a fresh check before taking payment (ADR-0218).
