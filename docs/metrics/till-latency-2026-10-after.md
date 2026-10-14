# Till latency after ADR-0173, October 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-10-14

Tap-to-paint in the till once the work of
[ADR-0173](../adr/0173-the-till-declares-each-piece-of-store-data-once.md) had landed. The figures
from before it are in [till-latency-2026-10.md](till-latency-2026-10.md). This file records one
moment and is not regenerated.

**Measured at** `fbc004a` on `main` (#761), plus this pull request's changes to the harness.

## How it was measured

The machine, browser, edge build and method are the ones in
[till-latency-2026-10.md](till-latency-2026-10.md):
- an Intel Xeon @ 2.80 GHz with 4 cores (`nproc` = 4) and 15 GB of memory;
- headless Chromium 141;
- a release `minimal-edge` serving a production build of the till;
- 10 rounds per run, nearest-rank percentiles.

**What changed in the harness:**
- **When a sample ends.** Adding a line, changing a quantity and moving a table are now painted
  before the edge answers, so their samples end at that paint.
- **Waiting outside the sample.** After each of those acts, the harness waits until the edge has
  confirmed it, so the samples stay independent.
- **Budgets.** Each act carries its ADR-0173 budget, which the nightly `tap-to-paint` job enforces
  with `POS_PERF_ENFORCE=1`.

## Tap-to-paint against its budgets

Two runs, one after the other. All figures are in milliseconds. "Headroom" is the budget minus the
larger of the two p95s.

| Act | Answered by | n | Run 1 p50 | p95 | max | Run 2 p50 | p95 | max | Budget (p95) | Headroom |
|---|---|---|---|---|---|---|---|---|---|---|
| Add a line | the till, painted before the edge | 100 | 11.1 | 12.6 | 14.4 | 11.3 | 12.5 | 16.0 | 50 | 37.4 |
| Change a quantity | the till, painted before the edge | 100 | 11.0 | 12.8 | 14.8 | 11.2 | 12.5 | 13.4 | 50 | 37.2 |
| Move a table | the till, painted before the edge | 50 | 13.7 | 19.5 | 22.5 | 12.9 | 16.8 | 17.9 | 50 | 30.5 |
| Order screen to floor | the till | 100 | 11.1 | 13.1 | 16.4 | 11.4 | 13.1 | 17.5 | 50 | 36.9 |
| Floor to order screen | the till | 100 | 12.2 | 18.2 | 31.4 | 12.3 | 15.9 | 18.0 | 50 | 31.8 |
| Open the move list | the till | 60 | 10.7 | 12.5 | 13.1 | 11.1 | 12.3 | 12.7 | 50 | 37.5 |
| Seat a table | the edge | 10 | 33.9 | 42.0 | 42.0 | 33.1 | 39.8 | 39.8 | 150 | 108.0 |
| Reload to a drawn order screen | the edge, for every read | 50 | 104.2 | 131.8 | 145.4 | 99.0 | 122.0 | 130.7 | 300 | 168.2 |

## Against the figures from before

| Act | p50 before (two runs) | p50 after (two runs) |
|---|---|---|
| Add a line | 29.1, 29.1 | 11.1, 11.3 |
| Change a quantity | 28.8, 29.0 | 11.0, 11.2 |
| Move a table | 29.8, 30.3 | 13.7, 12.9 |
| Reload to a drawn order screen | 109.0, 102.1 | 104.2, 99.0 |

The budgets are ADR-0173's own. The nightly runner is a different machine from this one, so its
figures and headroom will differ, and its job keeps them as an artifact for a fortnight.
