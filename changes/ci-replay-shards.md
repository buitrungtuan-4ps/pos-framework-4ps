---
kind: changed
---
- **The till's replay builds once and runs its flows in three shards** (ADR-0171 decision 2).
  - `replay-build` builds the UI and the `minimal-edge` binary that embeds it.
  - Three `replay-shard` jobs each run a third of the 130 flows against that binary
    (`pnpm replay --shard=k/3`). A failed shard keeps its traces as `replay-traces-<k>`.
  - `replay`, still the required check, passes only when all of them did, or when the change
    touched nothing the replay tests.
  - `playwright.config.mjs` sets `fullyParallel: true`, so one spec file splits by test. It keeps
    `workers: 1`, so flows still never overlap.
