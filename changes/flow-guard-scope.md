---
kind: fixed
upgrade_note: A flow's edge guards now apply by default only to orders its start act opened (`FLOW_GUARD_SCOPE_ENTERED`). A published flow that held back counter, takeaway or delivery orders it never started stops doing so; a package that wants that sets `guard_scope` to `FLOW_GUARD_SCOPE_ALL_ORDERS` or `FLOW_GUARD_SCOPE_CHANNELS`. `sales.order.opened` gains the optional field `opened_by_action_id`; an order open across the upgrade carries none and is not guarded.
---
- **A flow's guards apply only to the orders it governs** (ADR-0244, phase 10 open item O1). Each
  flow chooses `guard_scope`: only orders its start act opened (the default), orders on chosen
  channels, or every order. The edge records the act that opened each order and re-checks only the
  flows that govern it, and the till's waiting step follows the same rule. The console's flow
  designer offers the choice and warns when a guard can hold back orders outside the flow.
