# Till latency against 10 ms, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-03

The baseline for a 10 ms tap-to-paint target: every act the till's harness measures, at full speed
and with the CPU slowed four times, and where the time goes in the three slowest. The enforced
budgets are still [ADR-0173](../adr/0173-the-till-declares-each-piece-of-store-data-once.md)'s
(50, 150 and 300 ms) and this file changes none of them. Like
[till-latency-2026-10-after.md](till-latency-2026-10-after.md), it records one moment and is not
regenerated.

**Measured at** `a8b0bd4` on `main` (#1108), plus this pull request's changes to the harness, which
add measurements and change no code the measurements run through.

## How it was measured

- An Intel Xeon @ 2.10 GHz container with 4 cores (`nproc` = 4) and 15 GB of memory.
- Headless Chromium 153 (`chrome-headless-shell`, Playwright build 1243) at its default 60 Hz frame
  clock, through `POS_UI_CHROMIUM`.
- A release `minimal-edge` serving a production build of the till, as the nightly job runs it.
- `pnpm perf` ([`ui/perf/tap-to-paint.spec.mjs`](../../ui/perf/tap-to-paint.spec.mjs)), 10 rounds
  per run, two runs at each speed, nearest-rank percentiles.
- **4× is an emulation.** `POS_PERF_CPU_THROTTLE=4` sets Chrome DevTools Protocol
  `Emulation.setCPUThrottlingRate` on the page. It slows the page's main thread only: not the edge,
  not the compositor, and not the 60 Hz frame clock. It stands in for a low-end store PC until a
  reference machine exists, and it is not a measurement of one.
- The breakdown comes from one traced round per speed (`POS_PERF_TRACE`, read by
  [`ui/perf/trace-breakdown.mjs`](../../ui/perf/trace-breakdown.mjs)). Tracing has a cost of its
  own, larger at 4×, so its totals run above the untraced figures; read it for the proportions.

**What the harness now records beside tap-to-paint.** `tapToChange` is the time from the tap's
`pointerdown` to the moment the answering change is in the DOM, before the frame that paints it.
The difference between the two is the wait for the display's next frame plus that frame's style,
layout and paint. It is recorded and never enforced.

## The finding that decides how 10 ms is defined

**At 60 Hz, tap-to-paint as this harness measures it cannot reach 10 ms for any act, however fast
the till's code is.** At full speed every act the till answers alone has a tap-to-change p95 between
2.8 and 9.8 ms, and every one of them has a tap-to-paint p50 of 12.2 to 12.8 ms. The paint waits for
the next frame. A frame comes every 16.7 ms, and Playwright taps just after one, because its
"stable" check waits on animation frames before it clicks. So a tap whose work fits in the frame
lands at about 12.5 ms whatever that work costs. The trace agrees: in a typical "add a line" sample
the change and its layout are done 3 ms after the tap, and the main thread is then idle for about
9 ms until the next `BeginMainThreadFrame`.

A real touchscreen taps at a random point in the frame, so it would wait 8.3 ms on average and up to
16.7 ms. Either way, a p95 of 10 ms from the tap to the painted frame is not reachable at 60 Hz.
Whatever defines the 10 ms budget has to measure the part the till controls: the time to the change,
or to a frame that is ready to paint, with a separate check that the change makes the first frame
after it.

## Per act, at full speed and at 4×

All figures in milliseconds. Each cell gives run 1 / run 2. "Change" is tap-to-change.

| Act | n | 1× paint p50 | 1× paint p95 | 1× change p95 | 4× paint p50 | 4× paint p95 | 4× change p95 |
|---|---|---|---|---|---|---|---|
| Add a line | 100 | 12.6 / 12.6 | 13.6 / 13.5 | 4.4 / 4.3 | 14.9 / 16.1 | 31.6 / 27.7 | 22.2 / 21.3 |
| Change a quantity | 100 | 12.4 / 12.6 | 13.7 / 13.6 | 3.5 / 4.1 | 16.1 / 15.9 | 24.3 / 25.1 | 16.4 / 17.8 |
| Order screen to floor | 100 | 12.4 / 12.7 | 13.7 / 13.7 | 3.9 / 4.2 | 20.4 / 19.9 | 31.8 / 28.1 | 19.8 / 18.0 |
| Floor to order screen | 100 | 12.5 / 12.4 | 13.7 / 13.3 | 6.5 / 6.6 | 35.9 / 36.6 | 47.5 / 49.8 | 30.1 / 33.4 |
| Open the move list | 60 | 12.4 / 12.2 | 13.7 / 13.6 | 3.3 / 2.8 | 11.2 / 11.3 | 17.7 / 19.6 | 11.4 / 13.2 |
| Move a table | 50 | 12.4 / 12.8 | 13.7 / 14.6 | 9.8 / 9.8 | 44.2 / 40.9 | 56.3 / 55.5 | 42.4 / 47.3 |
| Seat a table | 10 | 16.9 / 17.2 | 22.1 / 24.2 | 16.2 / 19.5 | 74.6 / 76.9 | 92.5 / 90.7 | 66.8 / 66.6 |
| Reload to a drawn order screen | 50 | 67.5 / 63.6 | 78.9 / 87.7 | — | 265.2 / 266.6 | 340.4 / 336.8 | — |

Every figure is inside ADR-0173's budgets at 1×. At 4× the reload's p95 (340 ms) is over its 300 ms
budget. No other act is over its budget.

## Which acts are under 10 ms

| Act | Tap-to-paint p95 ≤ 10 ms | Tap-to-change p95 ≤ 10 ms at 1× | at 4× |
|---|---|---|---|
| Add a line | no (frame clock) | **yes**, 4.4 | no, 22.2 |
| Change a quantity | no (frame clock) | **yes**, 4.1 | no, 17.8 |
| Order screen to floor | no (frame clock) | **yes**, 4.2 | no, 19.8 |
| Floor to order screen | no (frame clock) | **yes**, 6.6 | no, 33.4 |
| Open the move list | no (frame clock) | **yes**, 3.3 | no, 13.2 |
| Move a table | no (frame clock) | **yes, by 0.2 ms**, 9.8 | no, 47.3 |
| Seat a table | no | no, 19.5: it builds the whole order screen | no, 66.8 |
| Reload to a drawn order screen | no | not a tap; reads from the edge | no |

At full speed, six of the seven taps already make their change within 10 ms. Seating a table does
not. Since [ADR-0188](../adr/0188-seating-a-free-table-is-painted-at-once.md) it no longer waits for
the edge, but the tap builds the whole order screen before anything changes. At 4× no tap is under
10 ms. On the emulated low-end machine the CPU cost of building a screen is the gap, not waiting on
the edge.

## Where the time goes in the three slowest taps

The slowest three by tap-to-change are **seat a table**, **move a table** and **floor to order
screen**. The reload is not a tap and has its own 300 ms budget. Its trace is in the last row for
completeness. Below are means over the traced round's samples, in milliseconds of the main thread's
own time inside each sample.

- **script**: event handlers, Solid's updates, microtasks, timers and GC.
- **style/layout**: style recalculation, layout and hit testing.
- **paint**: pre-paint, paint, layerize and commit.
- **other**: scheduling, IPC and tracing.
- **idle**: the main thread waiting. **frame** is the part of idle spent waiting for the next frame
  once the work was done.
- **`/api`**: how much of the sample had a request in flight. It overlaps the other columns and does
  not add to them.

| Act | Speed | n | total | script | style/layout | paint | other | idle | frame | `/api` in flight |
|---|---|---|---|---|---|---|---|---|---|---|
| Seat a table | 1× | 1 | 26.6 | 19.3 | 5.7 | 0.3 | 1.2 | 0.2 | 0.0 | 23.0 |
| Seat a table | 4× | 1 | 141.8 | 108.7 | 24.7 | 2.5 | 5.9 | 0.0 | 0.0 | 123.7 |
| Move a table | 1× | 5 | 12.7 | 6.9 | 1.8 | 0.3 | 1.3 | 2.5 | 0.6 | 10.6 |
| Move a table | 4× | 5 | 55.3 | 41.5 | 8.2 | 1.6 | 3.8 | 0.3 | 0.0 | 40.6 |
| Floor to order screen | 1× | 10 | 12.4 | 6.7 | 2.3 | 0.4 | 1.4 | 1.6 | 0.5 | 10.1 |
| Floor to order screen | 4× | 10 | 54.9 | 33.9 | 11.6 | 2.0 | 6.4 | 1.0 | 0.0 | 41.5 |
| *Add a line, for comparison* | 1× | 10 | 12.0 | 2.6 | 0.9 | 0.2 | 1.8 | 6.4 | 2.7 | 9.1 |
| *Reload to a drawn order screen* | 1× | 5 | 209.9 | 41.6 | 4.1 | 2.4 | 153.7 | 8.2 | 2.1 | 54.2 |

What this says:

- **Script dominates every slow tap, and style/layout is second.** Paint is under 3 ms even at 4×.
  The cost is in building the screen the tap leads to, not in drawing it.
- **Seating a table no longer waits for the edge, but it builds the most.** Since ADR-0188 the tap
  opens the order screen at once, and `POST /api/tables/:id/seat` runs alongside it: the request is
  in flight for 23 of the sample's 27 ms but is not on the critical path. The sample is the order
  screen being built from nothing (menu, categories, buttons): 19 ms of script and 6 ms of
  style/layout at 1×, 109 ms and 25 ms at 4×. Only drawing less in the first frame, then the rest,
  shortens it.
- **Moving a table and opening the order screen are both "render the order screen".** Each has
  7 ms of script at 1× and 34–42 ms at 4×. They have requests in flight in the same window, the
  reads that refresh the order, but the change is painted without waiting for them. Their cost
  scales with how much the order screen draws at once, which the next phase's renderer decides.
- **The cheap taps are cheap.** Adding a line, changing a quantity and opening the move list spend
  1–3 ms of script. At 1× most of their sample is the wait for the frame.

## Reproducing

```sh
cd ui && pnpm build && cargo build -p minimal-edge --release
POS_EDGE_BIN=../target/release/minimal-edge pnpm perf                            # 1×
POS_EDGE_BIN=../target/release/minimal-edge POS_PERF_CPU_THROTTLE=4 pnpm perf    # 4×, emulated
POS_EDGE_BIN=../target/release/minimal-edge POS_PERF_ROUNDS=1 POS_PERF_TRACE=trace.json pnpm perf
node perf/trace-breakdown.mjs trace.json
```

The figures land in `test-results/tap-to-paint.json` (or `POS_PERF_OUT`) under `tapToPaint` and
`tapToChange`, with the rate under `cpuThrottle`.
