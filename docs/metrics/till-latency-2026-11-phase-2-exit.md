# Till latency at the end of phase 2, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-06

The exit proof of roadmap-v4 phase 2 (runtime): the default package draws the till, and every act
the till's harness measures passes the 10 ms gate of
[ADR-0208](../adr/0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md). Like
[till-latency-2026-11-baseline.md](till-latency-2026-11-baseline.md), it records one moment and is
not regenerated.

**Measured at** `53d1c76` on `main` (#1202, the last pull request of the phase).

## How it was measured

- The metric is ADR-0208's: from the tap's `pointerdown` to the frame that answers it being ready to
  paint (`frameReady`), per act, nearest-rank percentiles.
- **2× is a stand-in.** `POS_PERF_CPU_THROTTLE=2` slows the page's main thread through Chrome
  DevTools Protocol `Emulation.setCPUThrottlingRate`. It does not slow the edge, the compositor or
  the frame clock, and it stands in for a store PC until a reference machine is measured (ADR-0208
  decision 4).
- **CI** is the `perf-gate` job of the `pr` workflow (`ui/perf/gate.mjs`): a release `minimal-edge`
  and a production till build on a GitHub-hosted runner with 2 cores. It blocks a pull request when an
  act's p50 is over 10 ms in two runs, and records p95 and long frames.
- **Local** is a 4-core Intel Xeon @ 2.10 GHz container, shared with other work. Headless Chromium
  (Playwright build 1243) runs the same harness against the same builds, 20 rounds, two runs.

## CI's perf gate (enforced)

The `perf-gate` job of the `pr` run on `53d1c76`, one run, every act passing on its first run:

| Act | n | p50 (ms) | p95 (ms) | Long frames |
|---|---:|---:|---:|---:|
| ask the guest count | 100 | 2.9 | 5.3 | 0 |
| seat a table | 100 | 5.0 | 8.7 | 0 |
| add a line | 200 | 3.8 | 7.8 | 0 |
| change a quantity | 200 | 3.4 | 4.7 | 0 |
| order screen to floor | 200 | 4.6 | 5.6 | 0 |
| floor to order screen | 200 | 5.3 | 6.2 | 0 |
| open the move list | 120 | 3.6 | 5.9 | 0 |
| move a table | 100 | 5.7 | 7.7 | 0 |

Every p95 is under 10 ms as well, which ADR-0208 records and does not yet enforce. Over the phase's
last merges (`7da30b5` to `53d1c76`), CI's p50s ran from 2.7 to 6.6 ms.

## Local (recorded)

Two runs at 2× on `53d1c76`, p50 and p95 in ms:

| Act | n | Run 1 p50 | Run 1 p95 | Run 2 p50 | Run 2 p95 |
|---|---:|---:|---:|---:|---:|
| ask the guest count | 20 | 7.6 | 11.7 | 7.6 | 9.4 |
| seat a table | 20 | 14.6 | 17.4 | 14.6 | 18.6 |
| add a line | 200 | 6.5 | 12.9 | 6.5 | 11.9 |
| change a quantity | 200 | 6.1 | 9.3 | 5.8 | 9.4 |
| order screen to floor | 200 | 8.2 | 12.5 | 8.3 | 12.4 |
| floor to order screen | 200 | 9.0 | 14.0 | 8.7 | 13.2 |
| open the move list | 120 | 5.9 | 11.2 | 5.9 | 11.7 |
| move a table | 100 | 9.6 | 15.5 | 9.6 | 13.9 |

**This machine is about twice as slow as CI's runner at the same throttle**, and seating a table is
over 10 ms here at p50. The same figure on `7da30b5`, before the phase's last three pull requests,
was 13.9 ms, so the reactions and the guest count on the live read do not move it. The gate is CI's
by ADR-0208 decision 3, and its rate is set against a reference NUC in phase 9 (decision 4). Seating
stays the act to watch: its tap seats the table, opens the order and draws the order screen in one
frame.

## Replays

The full replay suite (`ui/tests/replay.spec.mjs`, 228 flows) against an unstamped `minimal-edge`,
built the way CI's `replay-build` job builds it, on `53d1c76`: 228 of 228 normally, and 228 of 228
pinned to two CPUs (`taskset -c 0,1`), as CI's runners have. CI's three replay shards on `53d1c76`: green.

## What phase 2 shipped

Pull requests #1180 to #1203: the typed action catalogue, the screen renderer and the default
package, paint-first for every act with the money safeguards, the 10 ms gate (#1196), the rules
module in the till (#1197), the till running its package's flows with the guest count as a flow step
(#1198), reactions checked on apply (#1199) and run on the till (#1202), the guest count on the live
read (#1203), and the replay fixes the 2-core runners asked for (#1200, #1201).
