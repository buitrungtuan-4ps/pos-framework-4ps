# Till latency, October 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-10-14

The "before" figures for [ADR-0173](../adr/0173-the-till-declares-each-piece-of-store-data-once.md):
tap-to-paint in the till, and the edge round trip as the browser and the edge each see it. Like
[the October baseline](baseline-2026-10.md), this file records one moment. It is not regenerated;
a later measurement goes in a file of its own.

**Measured at** `501938a` on `main` (#754), plus the harness this file arrives with. The harness
changes no code the measurements run through.

## Machine

| | |
|---|---|
| CPU | Intel Xeon Processor @ 2.80 GHz, 4 cores, 1 thread per core (`nproc` = 4) |
| Memory | 15 GB |
| Kernel | Linux 6.18.44 (a cloud VM) |
| Browser | Chromium 141.0.7390.37, headless, Playwright's "Desktop Chrome" profile |
| Driver | `@playwright/test` 1.64.0, Node 22.22.2 |
| Edge | `minimal-edge`, release build, in-memory fakes, the demo store, on `127.0.0.1` |
| Till | production build (`pnpm build`), served by that edge |

Nothing else ran on the machine during each measurement. Browser and edge share the 4 cores, and
every request goes over loopback, so no network latency is included.

## 1. Tap-to-paint in the till

**Tool.** [`ui/perf/tap-to-paint.spec.mjs`](../../ui/perf/tap-to-paint.spec.mjs), run with
`pnpm perf` (`POS_EDGE_BIN` set to the release `minimal-edge`, `POS_UI_CHROMIUM` set to the browser).

**Method.**
- **One sample** runs from the tap's `pointerdown` timestamp, read by the page itself, to the first
  `requestAnimationFrame` callback after a `MutationObserver` sees the answering change in the DOM.
- **A reload** runs from the navigation's start to the first frame in which the order screen shows
  both the menu and the order's line.
- **Rounds.** Each run makes 10 rounds, and each round boots a fresh edge, pairs, and signs in.
- **One round:**
  - seats a table;
  - adds 10 lines (the first item on the grid);
  - taps **+** on the first line 10 times;
  - switches to the floor and back 10 times;
  - reloads the order screen 5 times;
  - moves the table until no table is free.
- **Percentiles** are nearest-rank. The figures are in milliseconds.

Two runs, one after the other:

| Act | What answers it | n | Run 1 p50 | p95 | max | Run 2 p50 | p95 | max |
|---|---|---|---|---|---|---|---|---|
| Seat a table | the edge | 10 | 32.7 | 44.2 | 44.2 | 33.7 | 39.3 | 39.3 |
| Add a line | the edge | 100 | 29.1 | 30.7 | 33.8 | 29.1 | 31.1 | 33.1 |
| Change a quantity | the edge | 100 | 28.8 | 30.0 | 35.7 | 29.0 | 30.7 | 38.1 |
| Move a table | the edge | 50 | 29.8 | 31.6 | 36.5 | 30.3 | 33.6 | 46.6 |
| Order screen to floor | the till | 100 | 11.0 | 12.8 | 14.0 | 11.1 | 12.5 | 13.8 |
| Floor to order screen | the till | 100 | 12.1 | 14.7 | 18.5 | 12.2 | 15.5 | 18.6 |
| Open the move list | the till | 60 | 10.8 | 12.4 | 13.1 | 10.7 | 12.3 | 12.8 |
| Reload to a drawn order screen | the edge, for every read | 50 | 109.0 | 138.7 | 145.6 | 102.1 | 141.5 | 177.2 |

## 2. The edge round trip, as the browser sees it

**Method.** The browser's resource timing (`responseEnd − startTime`) for every `/api/*` request
made during the same two runs. Ids in a path are folded to `:id`. A reload's reads go out together,
so their figures include waiting on each other. The `check` reads come from every act on the order
screen.

| Request | n | Run 1 p50 | p95 | max | Run 2 p50 | p95 | max |
|---|---|---|---|---|---|---|---|
| `POST /api/tables/:id/seat` | 10 | 2.0 | 2.9 | 2.9 | 2.0 | 4.5 | 4.5 |
| `POST /api/tables/:id/lines` | 100 | 2.3 | 3.8 | 5.2 | 2.3 | 3.6 | 6.7 |
| `POST /api/lines/:id/quantity` | 100 | 2.2 | 3.6 | 6.9 | 2.2 | 3.1 | 3.4 |
| `POST /api/tables/:id/transfer` | 50 | 2.2 | 4.4 | 5.9 | 2.3 | 3.5 | 4.7 |
| `GET /api/tables/:id/check` | 510 | 2.3 | 6.6 | 12.5 | 2.3 | 5.4 | 14.5 |
| `GET /api/floor` | 50 | 6.2 | 12.2 | 15.3 | 5.6 | 13.3 | 19.9 |
| `GET /api/menu` | 50 | 9.4 | 16.3 | 23.0 | 8.3 | 16.2 | 24.9 |
| `GET /api/menu?channel=SALES_CHANNEL_TAKEAWAY` | 50 | 11.3 | 16.7 | 23.6 | 9.9 | 18.6 | 26.0 |
| `GET /api/layout` | 50 | 11.1 | 17.2 | 23.8 | 10.6 | 18.7 | 26.2 |
| `GET /api/locale` | 50 | 11.1 | 18.1 | 23.9 | 11.5 | 18.9 | 26.5 |
| `GET /api/reason-codes` | 50 | 11.0 | 19.2 | 24.0 | 11.1 | 19.1 | 26.6 |
| `GET /api/session` | 100 | 8.0 | 18.7 | 30.9 | 6.5 | 17.5 | 30.1 |
| `GET /api/shifts` | 50 | 13.9 | 24.4 | 30.9 | 12.2 | 26.3 | 30.8 |
| `GET /api/shifts/current` | 50 | 14.5 | 24.2 | 30.8 | 13.9 | 25.9 | 31.0 |
| `GET /api/orders/live` | 50 | 14.5 | 24.3 | 30.7 | 13.3 | 26.5 | 30.9 |
| `GET /api/orders/kitchen` | 50 | 2.6 | 5.7 | 6.6 | 2.6 | 5.2 | 6.1 |
| `GET /api/activation` | 50 | 5.4 | 12.9 | 19.2 | 4.7 | 11.0 | 15.1 |
| `GET /api/sync` | 50 | 5.1 | 13.0 | 18.7 | 4.5 | 11.1 | 14.2 |

## 3. The edge's own hot paths

**Tool.** [`crates/pos-edge/tests/perf_budget.rs`](../../crates/pos-edge/tests/perf_budget.rs), the
nightly `bench` job's test:

```text
cargo test -p pos-edge --release --test perf_budget -- --ignored --nocapture
```

**Method.**
- **Inside the process.** Requests go through the edge's router in the same process, with no
  socket and no browser.
- **The store behind it:** 30,000 settled orders, and 40 live tables of 5 fired lines each.
- **Samples:** 300 for each path.
- **The contended case** adds a line every 3 ms while another task reads `GET /api/orders/live` in
  a loop.
- **Percentiles** are the sorted sample at index `n × q / 100`. This run added a max column to the
  report; the budgets and their assertions are unchanged.

| Path | Store | p50 | p95 | max | Budget (p95) |
|---|---|---|---|---|---|
| `GET /api/orders/live` | in-memory | 2.47 | 2.67 | 2.92 | 20 |
| Add a line | in-memory | 0.06 | 0.09 | 0.17 | 5 |
| Add a line, another device reading | in-memory | 0.27 | 0.94 | 2.55 | 10 |
| `GET /api/orders/live` | SQLite | 1.06 | 1.17 | 1.79 | 20 |
| Add a line | SQLite | 0.28 | 1.31 | 29.50 | 5 |
| Add a line, another device reading | SQLite | 0.69 | 1.76 | 22.17 | 10 |

Seeding the history took 15.2 s over the in-memory store and 109.5 s over SQLite.

## Observations

- **Acts that wait for the edge** (seat, add a line, change a quantity, move a table) paint at a
  p50 of 29–34 ms. The write request inside each one has a p50 of 2.0–2.3 ms in the browser's
  resource timing.
- **Acts the till answers alone** (switching screens, opening the move list) paint at a p50 of
  11–12 ms.
- **A reload** of the order screen paints at a p50 of 102–109 ms and a p95 of 139–142 ms. Its
  `/api/*` reads go out together, with p50s from 2.6 ms to 14.5 ms.
- **Inside the edge**, in a release build, the p95s of the three hot paths are between 0.09 ms
  and 2.67 ms.
- **Between the two runs**, the p50s differ by at most 7 ms, on the reload, and by at most 1.7 ms
  on any request.
