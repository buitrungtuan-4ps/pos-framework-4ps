---
kind: added
upgrade_note: "`sales.order.opened` gains an optional `tab_number`, absent on every other order. `POST /api/orders` takes an optional `tab: true`, refused with `CAPABILITY_DISABLED` on a store without `tabs_enabled`; a new read route, `GET /api/tabs`, and a new act, `sales.tab.open`. A tab's number comes from the counter's daily queue-number sequence."
---
- **A bar opens a tab and every till finds it by its number** (ADR-0221). On a store with
  `tabs_enabled`, the edge opens a tab as a tableless order numbered for the business day, lists
  the open tabs with what each owes and when its last round went out, and drops a tab once it is
  settled. The till's tab screen follows.
