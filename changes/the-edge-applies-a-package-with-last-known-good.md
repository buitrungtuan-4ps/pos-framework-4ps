---
kind: added
---
- **The store server applies its package version, or keeps the previous configuration** (ADR-0189).
  A pulled configuration whose package version the store server cannot run (malformed, breaking a
  publish rule, or needing a newer release) is refused whole. The store keeps trading on the
  configuration it had, and the refusal is recorded for the heartbeat to report. A version it can
  run is held on the store's session for the screens and flows that use it.
