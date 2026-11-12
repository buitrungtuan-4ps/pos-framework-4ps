# ADR-0212 — The heartbeat names the package version each till last read

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-11
· Relates to [ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0195](0195-the-cloud-wakes-a-store-to-pull-its-config.md)

Records a decision already shipped in #1241, at the owner's request.

## Context

A till that misses a `config_applied` keeps drawing its old package version, and the console could
see only the store's version, not each till's (roadmap-v3 B7.3).

## Options considered

1. **Add a list to the store's existing heartbeat** (chosen). The edge already knows which version
   it served to each paired till on `GET /api/package`.
2. **A new device-to-cloud call per till.** Tills never dial the cloud (AGENTS.md §1, rule 3), so
   this would break the outbound-only rule.
3. **An event per read.** Reads are not business facts, and events are the source of truth; it
   would flood the outbox with noise.

## Decision

- The heartbeat body gains an optional `devices: [{device_id, package_version_id, read_at_ms}]`,
  one entry per paired till, at most 64, the edge holding them in memory only.
- The cloud refuses more than 64 entries, a repeated till, or over-long ids, and keeps the last
  list per store. The fleet marks a till `behind` when it read another version than the store runs.
- It is additive on the device API. No `pos-proto` type, port, event, money or personal data
  changes: a `device_id` names a machine, never a person.

## Consequences accepted

- One nullable jsonb column on `store_liveness` (migration 0094).
- What the console shows is as fresh as the last heartbeat, not live.
