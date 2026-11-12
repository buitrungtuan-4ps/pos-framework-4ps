---
kind: added
upgrade_note: Migration 0095 adds a nullable `device_class` to `device_proposals`; every row reads null, so every store's devices node is published as before. `POST /admin/devices/proposals/{id}/device-class` is new, under `console.devices.manage` and If-Match, audited as `device_proposal.set_device_class`.
---
- **HQ says which kind of device each till is.** On the Devices screen, a terminal's Device class
  can be set to terminal, tablet, phone, kitchen display or kiosk. Once published, the till bound to
  it draws each package screen in the layout for that class instead of guessing from its screen's
  size. Left empty, it guesses as before.
