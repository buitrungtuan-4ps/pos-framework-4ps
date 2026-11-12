---
kind: added
upgrade_note: Migration 0097 adds `webhook_endpoints.event_types` (text array, default empty). Every endpoint registered before it keeps receiving every event.
---
- **A webhook can name the events it is sent.** Registering an endpoint in the console (or on
  `POST /admin/webhooks`, field `event_types`) may list event types such as `sales.order.opened`;
  the endpoint then receives only those, and the listing shows them. A type the event catalogue
  does not hold is refused. Naming none keeps every event, as before.
