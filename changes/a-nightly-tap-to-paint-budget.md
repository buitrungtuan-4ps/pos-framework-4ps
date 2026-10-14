---
kind: added
---
- **A nightly tap-to-paint budget for the till** (ADR-0173). The `tap-to-paint` job in
  `nightly.yml` runs a release store server and a production till in headless Chromium. It fails
  when an act's 95th percentile exceeds its budget: 50 ms for what the till answers alone or paints
  before the store answers, 150 ms for an act that waits for the store, and 300 ms from a reload.
  It never runs on a pull request. The figures after ADR-0173's work are in
  `docs/metrics/till-latency-2026-10-after.md`.
