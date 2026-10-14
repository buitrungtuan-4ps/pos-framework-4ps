# The till's store data

**Status** Done · **Owner** @maintainers-architecture · **Last updated** 2026-10-14

## Goal

The till declares each piece of store data once and loads it through one engine. It says when
part of it will not load, keeps a snapshot of its configuration only, paints three acts before the
edge answers, and is held to a tap-to-paint budget every night
([ADR-0173](../adr/0173-the-till-declares-each-piece-of-store-data-once.md)).

## Steps

| Step | What | Status | Pull requests |
|---|---|---|---|
| 1 | ADR-0173 | Done | #754 |
| 2 | The figures from before ([till-latency-2026-10.md](../metrics/till-latency-2026-10.md)) and the harness | Done | #755 |
| 3 | The edge: an `ETag` and `304` on the five configuration reads, and `changed_nodes` on `config_applied` | Done | #756 |
| 4 | The registry and engine: configuration (4a), live data (4b), each screen's own reads (4c) | Done | #757, #758, #759 |
| 5 | The configuration snapshot | Done | #760 |
| 6 | Optimistic writes for adding a line, changing a quantity and moving a table | Done | #761 |
| 7 | The nightly `tap-to-paint` job and the figures after ([till-latency-2026-10-after.md](../metrics/till-latency-2026-10-after.md)) | Done | this pull request |

## Decisions taken

- 2026-10-14 — The owner delegated approval of ADR-0173 (#754).
- 2026-10-14 — The edge's tags are weak, because it serves one body gzip or plain under one tag
  (#756).
- 2026-10-14 — The snapshot is kept across the idle lock and cleared on sign-out and unpairing
  (#754).
- 2026-10-14 — The floor is not kept in the snapshot: its read carries live table states (#760).
- 2026-10-14 — The status bar's `GET /api/sync` poll stays outside the engine, because its timer
  is its retry (#759).
- 2026-10-14 — The add-line answer is not extended with `line_total`; a line shows "Sending…"
  until the edge's event brings its figure (#761).

## Open questions

None.

## Links

- [ADR-0173](../adr/0173-the-till-declares-each-piece-of-store-data-once.md)
- [ADR-0109](../adr/0109-counting-the-taps-an-operator-makes.md), which keeps timing out of every
  commit's checks
- [`docs/ui-ux.md`](../ui-ux.md) §4, which describes the behaviour
