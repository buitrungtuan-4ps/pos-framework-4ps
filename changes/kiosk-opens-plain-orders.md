---
kind: security
---
- **A kiosk opens only an eat-in or takeaway order.** `POST /api/orders` from a kiosk's own record
  is refused `403 KIOSK_NOT_ALLOWED` for a tab or for any other channel, which would change the
  order's tax; a person on a till is unaffected. The `/ws` and `/api/orders` OpenAPI entries now
  document the kiosk refusals and `/ws`'s `503` when a kiosk binding cannot be read (phase 9
  security review, findings K1 and K3).
