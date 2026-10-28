---
kind: added
---
- **An HQ can name the cells it reads rollups from.** An optional `[federation]` section in
  `cloud.toml` lists other cells, each with an API key that cell issued with the `read_rollups`
  scope and the stores to read there (ADR-0179 C10). It is checked at boot: a malformed cell id, a
  blank key, a store that is not a ULID, a cell named twice or more than 64 sources stop the process
  naming the source, and at most 2,000 stores are read in all. Keys never reach a log and are never
  committed: they live in the box's `cloud.toml` only. **Only aggregates cross a cell boundary**: a
  store's daily count of events, in total and by event type. No personal data, no `subject_id`, no
  row about a person and no raw event is read from another cell, and a test fails if the rollup
  gains any field. A cell's address is never configured here: it comes from
  the signed cell directory. The fold that reads every source, and reports a source that fails
  without failing the others, exists as a library; the HTTPS fetch and the console view follow.
