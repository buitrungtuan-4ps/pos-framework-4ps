---
kind: changed
---
- **A store on a cloud without archive storage says so once, not as a warning every round.** The
  cloud answers the archive-key request `404` when it has no archive storage. `cloud-sync-http` now
  reports that as `failed_precondition` rather than `unavailable`
  ([ADR-0185](../docs/adr/0185-the-cloud-says-when-it-keeps-no-archives.md)). The edge logs it once
  per start at INFO, skips the round, and keeps asking each interval. A cloud it cannot reach is
  still a WARN every round.
