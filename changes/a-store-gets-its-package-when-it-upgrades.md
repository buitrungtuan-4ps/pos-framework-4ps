---
kind: added
---
- **A store that waited for an edge update gets its package version when it reports the update**
  (ADR-0189). When a store reports a release at least as new as its pinned version needs, the
  cloud republishes that store's settings by itself, so the version reaches the store on its next
  pull with nobody pressing publish. A store already running its version is not republished.
