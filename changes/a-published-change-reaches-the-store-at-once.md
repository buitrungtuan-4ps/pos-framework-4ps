---
kind: added
---
- **A published change reaches a store at once instead of on its next 30-second poll** (ADR-0195).
  When the cloud and the store both use the event bus, every saved store configuration wakes its
  store, which pulls straight away and again whenever it reconnects; a store without the bus keeps
  the poll. The console's task health shows the cloud's `config_wake` sender with its sent, dropped
  and failed counts, and each wake-driven apply is logged on the store with how long it took.
