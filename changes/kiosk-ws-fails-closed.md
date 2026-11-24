---
kind: security
---
- **`/ws` refuses a device whose kiosk binding cannot be read.** On a store that publishes a kiosk
  terminal, a paired device whose terminal binding cannot be read now gets `503` on `/ws` instead
  of the store's whole event stream, so a kiosk is never let in on a guess. A store with no kiosk
  terminal no longer reads any binding to answer `/ws` or a sign-in (phase 9 security review,
  finding K1).
