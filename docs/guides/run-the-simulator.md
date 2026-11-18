# Run the simulator

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-11-18

`pos-simulator` turns the sizing numbers in [`capacity-and-reliability.md`](../capacity-and-reliability.md)
into something you can run, drives a virtual fleet through the framework's real decisions, and sells
each business-type template in each country pack. It is offline — no hardware, no network — and all
but the matrix is deterministic, with no clock.

```bash
just simulate          # or: cargo run -q -p pos-simulator
```

It prints two things:

- **The capacity envelope** — the three published scenarios (A/B/C), each showing what the model
  derives (events/day, PostgreSQL disk, bandwidth, peak-ingest ceiling) next to what the doc states.
- **The reconciliation report** — where the model and the published table disagree. Today that is one
  pinned finding (scenario A's QR-session count), left for the pilot to settle rather than papered over.

## What it proves (and where the checks live)

The numbers above are asserted in the crate's tests — `just simulate` is the human-readable view of
what CI already enforces:

```bash
cargo test -p pos-simulator
```

- `capacity` — the §2 sizing formulas reproduce the published tables (events/day exactly, disk within
  5%, bandwidth within each range, the peak-ingest formula shown to be a conservative ceiling).
- `fleet` — an OTA ring rollout run over the **real** `pos_core::ota::decide_rollout`: the canary ramp,
  the kill switch, a revoked key, a failed self-test rolling back — asserted across a whole fleet.
- `stress` — §4's behavioural tests: the offline drain (200 stores → 800k events → ~9 minutes,
  feasible within the ingest ceiling), the webhook backpressure (a dead endpoint's cursor falls behind
  while its in-memory footprint stays one batch), and the nightly reconciliation missing-id diff.

## The template-by-country matrix

```bash
just matrix                                  # all 24 cells
just matrix --template cafe                  # one template's row: cafe in vn, jp, in and zz
just matrix --country jp                     # one country's column: every template in Japan
just matrix --template bar --country in      # one cell
```

Every business-type template in [`business-types/`](../../business-types/) is sold in every shipped
country pack ([ADR-0200](../adr/0200-six-business-type-templates-ship-with-the-framework.md)
decision 6). Each cell is a store of its own: an in-process edge over the in-memory store
(`Edge<FakeStore>`), configured through `session_from_config` from the document the cloud would
publish (the pack's locale and tax table, the template's capability preset and recommended
settings, its demo menu priced in the pack's currency, its floor and kitchen stations, and the till
approved as a terminal). A till paired to it and signed in as a fake member of staff then drives
the edge's own HTTP routes: it opens a shift, sells two items the store can ring up (at the first
table where the template serves at tables, as a takeaway at the counter otherwise), sends them to
the kitchen where there is one, opens the bill, pays the check's total in cash and counts and closes
the shift. A cell passes when the bill settles with receipt number 1 and the drawer closes on the
amount paid with no variance; a failure names the step and the edge's answer, with its
`pos-error-reason`. The command exits non-zero when a cell fails, and `2` when the filter names no
shipped template or country. `cargo test -p pos-simulator` runs all 24 cells.

An item the pack does not price is left unsold, as a till cannot ring it up: India's pack has no
alcohol rate, because a state sets it, and neither does the neutral `zz`, so the bar sells its food
there. Self-order sells as table service and the bar without a tab until their own capabilities
land (ADR-0200 decision 5).

## What it is not

It does not measure real throughput, and the matrix runs no real device, database or network. The sustained soak — 222 events/s against a live PostgreSQL with
NVMe `fsync` the deciding factor, for hours — needs the target hardware and is a pilot/operations
exercise. This crate is the harness that soak plugs into; the number itself is confirmed at the pilot.
