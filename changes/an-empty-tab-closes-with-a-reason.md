---
kind: added
upgrade_note: "`sales.order.closed` gains an optional `reason_code_id`, absent on a released table, a refused guest order and every close before it. A new reason action, `REASON_ACTION_CLOSE_ORDER`, which the framework's default `CUSTOMER_CHANGED_MIND` and `STAFF_ERROR` now also cover; a store's own published list need not cover it, and closes without a reason until it does. A new route, `POST /api/orders/{id}/close`, a new act, `sales.order.close` (permission `sales.line.add`), and a new refusal, `CLOSE_REASON_NOT_VALID`."
---
- **A tab nothing was sold on can be closed, with a reason** (ADR-0221). A tab opened by mistake,
  or whose guest left before ordering, used to stay on every till's list for good. The edge now
  closes any open tableless order with no sold line and no open bill, recording the store's reason
  for closing; the console's reason list offers the new act. The till's button comes with its tabs
  screen.
