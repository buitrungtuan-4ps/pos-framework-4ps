---
kind: added
upgrade_note: "A new setting, `tabs.guest_label` (`GUEST_LABEL_OFF` by default, `GUEST_LABEL_ON` written only with a recorded confirmation, ADR-0169). A new route, `POST /api/tabs/{id}/label`, a new act, `sales.tab.label` (permission `sales.line.add`), and a new refusal, `GUEST_LABEL_OFF`. `GET /api/tabs` gains an optional `label`; `GET /api/menu` gains `tabs_enabled` and `tab_guest_label`. No event changes. A store owes its lawful basis under Decree 13/2023 and a DPIA entry before it turns the label on."
---
- **A bar tab may carry a guest's label, where the store turns it on** (ADR-0221). Staff may type up
  to 32 characters beside a tab's number to find it again. The label stays in the store's own
  subject store, never in an event, a log, a ticket or the cloud, and is masked after the store's
  retention days. It is off unless the store confirms its notice in the console's Settings; no
  template turns it on. The till's field comes with its tabs screen.
