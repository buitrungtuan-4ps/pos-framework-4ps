---
kind: added
---
- **POS Station has a kiosk mode on any Windows edition** (ADR-0201 decision 2): with `"kiosk": true`
  in `station.json`, the till is full screen, frameless and on top, cannot be closed with Alt+F4 or
  the taskbar, and the tray offers no Quit. Built and tested on Linux CI; not yet checked on a NUC.
