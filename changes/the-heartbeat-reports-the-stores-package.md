---
kind: added
upgrade_note: Migration 0090 adds five nullable columns to `store_liveness` (the package a store runs and the configuration it last refused for its package). A store reports them from its next heartbeat; an older edge reports nothing. Development, replay and simulator edges built through the justfile now say they are `dev_edge_release` (0.14.1) rather than 0.0.0, so they run package versions.
---
- **The fleet console shows which package version each store runs, and a configuration it refused**
  (ADR-0189). The store server's heartbeat reports both, and the store server answers them itself
  at `GET /api/package`. A refusal names the configuration and why: malformed, breaking a rule, or
  needing a newer store server.
