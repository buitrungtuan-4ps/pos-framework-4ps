---
kind: changed
upgrade_note: "`GET /api/menu` gains `pay_first_enabled`."
---
- **A pay-first counter takes payment without asking to send first** (ADR-0222). On a store with
  `pay_first_enabled`, the till no longer asks to send a counter order's unsent lines before
  payment: the settle sends them itself, so the kitchen sees the order once it is paid.
