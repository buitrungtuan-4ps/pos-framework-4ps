# Package delivery at the end of phases 0 and 1, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-10

The exit record of roadmap-v4 phase 0 (foundations) and phase 1 (package and distribution): what
each phase's exit criterion asked, what shows it is met, and what the phase's scope still leaves
open. Like [till-latency-2026-11-phase-2-exit.md](till-latency-2026-11-phase-2-exit.md), it records
one moment and is not regenerated.

**Measured at** `c4085e3` on `claude/g1-publish-e2e`, the branch that adds the timed test, in run
42859664745 of the `main` workflow dispatched on it.

## Phase 0: Milestone 1 is met

The exit criterion: the decisions are merged as Accepted and the baselines are recorded under
`docs/metrics/`.

| Asked for | Where it is | Met |
|---|---|---|
| The thirteen decisions | [ADR-0189](../adr/0189-a-store-runs-a-package-version-hq-pins.md) to [ADR-0201](../adr/0201-the-till-runs-on-every-device-class.md), each **Accepted** on 2026-11-03 | Yes |
| A tap-to-paint baseline per act at 1× and 4× | [till-latency-2026-11-baseline.md](till-latency-2026-11-baseline.md) (#1113) | Yes |
| Spike: CEL in the till (WebAssembly) and on the edge | `spike/cel-on-both-sides`, recorded by [ADR-0202](../adr/0202-cel-as-the-crate-and-the-spike-found-it.md) | Yes |
| Spike: a screen rendered from a package | `spike/a-screen-from-a-package` (`ui/src/package/FINDINGS.md`), then phase 2's renderer | Yes |
| Spike: a NATS wake | `spike/push-to-the-store` (`spikes/push/FINDINGS.md`), then #1151 to #1155 | Yes |
| Spike: Tauri v2 on Android and iPad | `spike/tauri-mobile-feasibility` (`spikes/tauri-mobile/FINDINGS.md`) records what a mobile build needs; **nothing was built**, because this environment cannot fetch the Android NDK and cannot build for iOS | **No.** It moves to phase 8, where roadmap-v4 already places ADR-0147's Android spike |
| Unit tests for `ui/` (gate A5) | #1114 | Yes |

Milestone 1's criterion names the decisions and the baselines, and both are met. The mobile spike
is the one piece of phase 0's scope with no result; ADR-0201 decision 1 still waits on it.

## Phase 1: the exit criterion is met

The exit criterion: a package version published in the console is running on a test edge within
one second, a store can be rolled back to the previous version, and an edge too old for a version
keeps the one it runs.

### Published to running within one second: met

[`dashboard/tests/publish-to-edge.mjs`](../../dashboard/tests/publish-to-edge.mjs), the
`publish-to-edge` job of the `main` workflow, runs every piece for real:

- the `pos_cloud` binary over its own PostgreSQL 16 database, with `[nats]` configured, so a
  committed publish queues a wake (ADR-0195 decision 1);
- a NATS 2.10 server with JetStream, the digest the `integration` job pins;
- the `pos-edge` binary, stamped with the justfile's `dev_edge_release`, activated with a code from
  the console, dialling the cloud over https and the broker with its wake on;
- the console's own `/admin` calls, signed in with a password and a TOTP code.

The harness locks the store to one version, untimed, then switches the lock between two versions
six times, so three of the six are rollbacks. Each time it measures from sending the settings write
to the edge saying `applied the configuration a wake announced` for the configuration that write
published. The edge says that only after applying the whole document, package included, and only
for a pull a wake started; a delivery by the 30-second poll fails the run.

Run 42859664745, on a GitHub-hosted `ubuntu-24.04` runner with debug builds:

| Round | Switch | Console write to running (ms) |
|---:|---|---:|
| 1 | to the second version | 118 |
| 2 | back to the first (rollback) | 117 |
| 3 | to the second | 75 |
| 4 | back to the first (rollback) | 114 |
| 5 | to the second | 74 |
| 6 | back to the first (rollback) | 77 |

p50 77 ms, p95 118 ms, against ADR-0195 decision 6's one second. The network round trip ADR-0195
adds to the target is the loopback's here. Because the job runs after merge (NATS needs a command a
`services:` block cannot pass), every merge to `main` measures it again, and the job summary shows
the figures.

### Rollback: met

Rolling back is pinning the previous version, delivered like any other publish (ADR-0189 decision
6). Proven by the compile test of #1133, the console flow of #1157 and the edge's apply of #1138, and
here end to end: rounds 2, 4 and 6 above are rollbacks, each running within 120 ms.

### An edge too old keeps what it runs: met

The cloud compiles a version only for a store whose reported release meets it, and keeps what the
store runs otherwise (#1133, #1136); the edge refuses a version above its own release as a whole
(#1138). The console shows why a store waits (#1158).

## Phase 1's scope that is still open

Roadmap-v4 folded three roadmap-v3 items into phase 1. None of them is in the exit criterion, and
none is finished:

- **B7.2, lock by path: done for `package` only.** A `PACKAGE_MODE_LOCKED` at a broad scope refuses
  a narrower `package` value with `409 PACKAGE_LOCKED_ABOVE` ([architecture.md](../architecture.md)).
  Not done: a lock on any other path (a brand pinning `tax`), its `409` for every node, and the
  console's lock marker on a locked setting.
- **B7.3, drift v2: partly done.** Done: the edge keeps the configuration it applied and restores it
  at start-up; the cloud keeps each store's held and published versions (`/admin/fleet`); the
  heartbeat carries the package the store runs, any package it refused, and how configuration
  reaches it and how fast (#1155). Not done: **per device**. Nothing records which configuration each
  paired till last read, so a till that missed its `config_applied` is invisible to the console.
- **B7.5, the edge's diagnostics: partly done.** `GET /api/package` answers which version the store
  runs and the last refusal. Not done: the read-only Settings and Diagnostics screen on the till,
  showing the effective configuration, where each value came from, and the gap to the published
  version.

## Found on the way

- **A cloud with `[nats]` would not start before a store had connected.** Its ingest cursor bound
  only an existing fleet stream, which the first store to connect creates (ADR-0087 Amendment 1), and
  refused to start without it (`message_link: the cloud consumer is unavailable`). The timed test
  found it. Fixed in #1224: the cursor creates the stream as a store would, and the timed test now
  starts the cloud first on a fresh broker, so every run proves it.
