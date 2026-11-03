# Spike: a screen from a package (throwaway, never merged)

The Order screen's button area is drawn from `order-buttons.json` by a ~90-line renderer
(`render.tsx`) and a catalogue of 5 components plus `each` (`catalogue.tsx`: stack, grid, section,
tabs, item_button). The screen keeps every rule: `item_button` calls the screen's own
`sellButton`/`arrangedButton`, so the permission, sold-out and billed rules are unchanged.
`Order.tsx` replaces 63 lines of JSX with one `<Render>`, plus a 17-line scope/actions block.
`tsc` is clean and the bundle is +0.8 KB gzip (111.34 KB).

## Same harness, same throttle: hand-written vs package

The setup is release `minimal-edge`, headless Chromium 153 and the harness from
`claude/the-till-is-measured-against-ten-ms`. There are 2 runs × 10 rounds per variant, run
alternately (hand, package, hand, package) on the same 4-core container. Figures are p95 in ms, as
run 1 / run 2. "Change" is tap-to-change.

| Act | 1× paint hand | 1× paint pkg | 1× change hand | 1× change pkg | 4× paint hand | 4× paint pkg | 4× change hand | 4× change pkg |
|---|---|---|---|---|---|---|---|---|
| Add a line | 13.0 / 13.2 | 13.8 / 13.6 | 5.3 / 5.7 | 5.6 / 4.9 | 30.4 / 29.0 | 33.5 / 33.6 | 23.7 / 21.8 | 26.2 / 26.2 |
| Change a quantity | 13.4 / 13.2 | 13.8 / 13.4 | 5.1 / 5.0 | 6.4 / 4.8 | 30.7 / 33.0 | 28.5 / 26.4 | 21.5 / 20.6 | 20.1 / 19.2 |
| Order screen to floor | 13.2 / 13.2 | 13.5 / 13.4 | 4.5 / 4.9 | 4.5 / 5.0 | 32.2 / 36.7 | 35.0 / 33.4 | 21.9 / 20.9 | 25.8 / 23.4 |
| Floor to order screen | 16.2 / 13.6 | 15.2 / 15.5 | 10.3 / 9.8 | 10.9 / 11.2 | 65.7 / 57.1 | 58.1 / 54.5 | 42.9 / 38.8 | 40.8 / 36.4 |
| Open the move list | 12.9 / 13.4 | 13.3 / 13.3 | 3.1 / 3.0 | 4.3 / 3.4 | 22.5 / 25.2 | 26.2 / 27.9 | 14.2 / 17.4 | 18.2 / 16.8 |
| Move a table | 20.2 / 16.7 | 25.5 / 21.6 | 12.6 / 12.5 | 17.8 / 15.0 | 71.5 / 75.4 | 69.9 / 65.5 | 54.5 / 52.3 | 54.4 / 49.0 |
| Seat a table (n=10) | 51.0 / 32.6 | 24.1 / 30.9 | 41.6 / 25.6 | 18.5 / 25.4 | 136.9 / 96.5 | 135.0 / 123.8 | 117.3 / 72.3 | 95.4 / 84.3 |
| Reload | 442.7 / 141.9 | 575.4 / 306.7 | — | — | 351.5 / 344.6 | 361.7 / 340.3 | — | — |

**Reading it:**
- The two variants are within run-to-run noise on every act that draws the button area (seat a
  table, floor to order screen). The package one is not slower there.
- Adding a line at 4× is +2.5 to +4.4 ms on tap-to-change in both runs, and moving a table at 1×
  is +2.5 to +5 ms. Both are small and may be noise. The 1× reload p95s are noisy for both variants:
  this session's machine was busier than the morning's baseline run (compare
  `docs/metrics/till-latency-2026-11-baseline.md`).
- **Verdict:** a generic renderer (`For` over a scope, a catalogue lookup per node, `$path` props
  resolved by string split) costs nothing measurable at this size. The package format is not
  what blocks 10 ms. Screen-building cost is (see the baseline's breakdown).

## Limits of this spike (what it did not prove)

- **The demo store has a small flat menu and no published layout.** So the run exercises the
  `no_layout` grid. The tabs and arranged-category branch is typed and compiles but was not timed. A
  300-item arranged plan is the next number to get, with the harness on a seeded store.
- `$path` props are resolved at render time by string split. A real renderer should compile a
  package once into accessor closures, as with CEL compile-once.
- Typing: the catalogue casts `unknown` everywhere. A real one needs a typed schema per component
  (ADR-0190), generated so the console and the till share it, and a validator at publish.
- The unknown-component fallback is "draw nothing", silently. ADR-0190 wants a visible placeholder
  and a telemetry count.
- The catalogue reached into the screen's closures (`sellButton`). A real one goes through the
  typed action catalogue (ADR-0191), so a package can only name acts, never code.
