---
kind: added
upgrade_note: A `TERMINAL` entry on the `devices` node gains an optional `device_class` (ADR-0201 decision 3), left off the wire while unset, and `GET /api/session` gains `device_class`, omitted when the device's till names none. The cloud publishes none yet; an older edge or till ignores both and keeps reading the class from the screen.
---
- **A till can be told which kind of device it is.** Where the till a device is bound to names a
  device class, the till draws a package screen's layout for that class instead of guessing it
  from the screen's width. A till whose terminal names none draws as before.
