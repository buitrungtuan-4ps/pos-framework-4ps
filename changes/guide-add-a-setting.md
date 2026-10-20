---
kind: added
---
- **A guide to adding a setting.** `docs/guides/add-a-setting.md` walks through a real setting,
  `session.idle_lock_seconds`, from its ADR to its field, register entry, edge reader and console
  words, quoting the code as it is, and lists the checks that prove it. A new setting changes
  `pos-proto`, so the guide asks for a short ADR first and gives a skeleton for one.
  `just guides-check` holds every quote to the code, and a test in `pos-edge` holds the register to
  what the guide says.
