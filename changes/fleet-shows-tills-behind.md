---
kind: added
upgrade_note: Store-postgres migration 0094 adds two nullable columns to `store_liveness` (`device_reads`, `device_reads_reported_at`); it is additive and applied on boot. Stores report their tills from the release that carries this change; an older edge reports none and its fleet row shows nothing for them.
---
- **The fleet shows a till that runs another package version than its store.** Each paired till's
  last read of its package is reported by the store's heartbeat, and the fleet table says how many
  of a store's tills are on another version, so a till that missed an update is no longer invisible
  (roadmap-v3 B7.3).
