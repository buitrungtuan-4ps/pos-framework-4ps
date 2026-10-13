---
kind: fixed
---
- **Paid in and Paid out no longer stay disabled after signing in on a busy till.** Right after a
  sign-in the till loads its reason codes, session, shift and orders at once. The edge stamped each
  request's time before taking the sign-in lock, so a request could find that a later one from the
  same till had already moved the device's last-seen time past its own. It read that as the clock
  going backwards, treated the sign-in as expired and answered `403`. When the refused request was
  the reason-code list, the till kept an empty list and both cash-movement buttons stayed disabled
  for the shift, even with a valid amount. The edge now reads the clock inside the same lock that
  moves the last-seen time. A clock that really does go backwards still expires the sign-in.
