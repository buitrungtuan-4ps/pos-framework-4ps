# Run the simulator

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21

`pos-simulator` turns the sizing numbers in [`capacity-and-reliability.md`](../capacity-and-reliability.md)
into something you can run, and drives a virtual fleet through the framework's real decisions. It is
deterministic and offline — no hardware, no clock, no network.

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

## What it is not

It does not measure real throughput. The sustained soak — 222 events/s against a live PostgreSQL with
NVMe `fsync` the deciding factor, for hours — needs the target hardware and is a pilot/operations
exercise. This crate is the harness that soak plugs into; the number itself is confirmed at the pilot.
