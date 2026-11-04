---
kind: added
upgrade_note: Migration 0091 adds five nullable columns to `store_liveness` (how configuration reaches the store, wake or poll, and its wake-to-apply times). A store reports them from its next heartbeat; an older edge reports nothing.
---
- **The fleet console shows how configuration reaches each store, and how fast** (ADR-0195). Each
  store's heartbeat says whether it hears the cloud's wake or runs on the 30-second poll alone, and
  the 95th-percentile and longest time from a wake to applying its version over its latest 128
  wakes.
