---
kind: added
upgrade_note: A new event, `sales.order_line.repriced`; a new reason action, `REASON_ACTION_PRICE_OVERRIDE`, which the framework's `SERVICE_RECOVERY` reason now also covers; a new edge route, `POST /api/lines/{id}/price`, and act, `sales.line.reprice`; a new refusal, `PRICE_OUT_OF_RANGE`; and a new setting, `billing.price_override_max_multiple`, default 100. A cloud reader that does not know the event or the action ignores them. `billing.price.override` stays PIN-flagged and manager-only by default.
---
- **A line's price can be overridden before its bill opens.** `POST /api/lines/{id}/price` sets a
  new price of one, above zero and at most the store's multiple of the price the line was added
  at (100 times unless the store sets less), for a reason the store publishes for an override and
  with a manager's approval where the person acting needs one (ADR-0219). The edge computes the
  new total; tax and fees follow it. The till's button for it is a later change.
