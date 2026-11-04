---
kind: added
---
- **The console says why a store does not run its pinned package version yet, and the fleet shows
  how configuration reaches each store** (ADR-0189, ADR-0195). With a store in context, *Packages ›
  Versions and pins* shows what it runs, what it is pinned to, and why it waits, such as an edge
  release older than the version needs. The fleet table gains **Config delivery** (wake or poll only,
  with the 95th-percentile and longest time from a wake to applying it) and **Package** (the version
  a store runs, and a configuration it refused).
