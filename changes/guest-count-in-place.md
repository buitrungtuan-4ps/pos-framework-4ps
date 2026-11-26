---
kind: added
upgrade_note: A new event `sales.order.guests_counted` and edge route `POST /api/tables/{id}/guests` (act `sales.table.count_guests`); a reader older than this release skips the event and keeps the count the seat carried.
---
- **The till asks for a missing guest count in place** (ADR-0254). Where the store's flow asks for
  the guest count before the first dish and a table was seated without it, the order screen now asks
  "How many guests?" with one tap per count, and the dishes can be added at once. Before this it said
  to give the table back and seat it again. The new act `sales.table.count_guests` (route
  `POST /api/tables/{id}/guests`, permission `sales.table.manage`) writes the new event
  `sales.order.guests_counted`; the latest count wins. Any other step a flow puts first is named by
  what its act does, never by its id.
