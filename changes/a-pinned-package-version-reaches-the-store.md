---
kind: added
---
- **A pinned package version reaches the store's document** (ADR-0189). Each settings publish
  compiles the version a store is locked to, or has chosen, into its `package` node. It does this
  only once the store has reported a release at least as new as the version's minimum edge release.
  Until then the store keeps the version it runs, and `GET /admin/stores/{store_id}/package` says
  why it waits: its release is too old or unknown, the version is missing, or the pin names none.
  Pinning the previous version rolls a store back, and removing the pin returns it to the default
  package.
