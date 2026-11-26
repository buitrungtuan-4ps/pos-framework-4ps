---
kind: added
upgrade_note: Three new settings on the `qr` node, `qr.auto_fire_from_minute`, `qr.auto_fire_until_minute` and `qr.kitchen_release_outside_hours`. Their defaults (0, 0, `KITCHEN_RELEASE_CONFIRM_THEN_SEND`) keep today's behaviour.
---
- **A QR order's automatic send to the kitchen can keep the store's hours** (ADR-0255). Under
  "Send on arrival" (`KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE`), a store can now limit the send with no
  tap to one daily window, in minutes after its local midnight; the window may wrap past midnight.
  An order that arrives outside the window waits for staff (the default) or is sent with the tap
  that accepts it, as `qr.kitchen_release_outside_hours` says. The edge reads the time from its own
  clock in the store's time zone. By default the window is always open, as before.
