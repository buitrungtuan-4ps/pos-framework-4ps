# `pos-simulator`

**Layer** `testing` · **Owners** @maintainers-architecture

## What it is

The capacity model in `docs/capacity-and-reliability.md` as code, and a virtual fleet driven through the framework's real decisions: an OTA ring rollout, an offline drain, a nightly reconciliation. It needs no hardware and no clock. The real soak on target hardware is not here.

## Owns

- The capacity model and its check against the published tables
- Fleet-scale OTA rollout scenarios over `pos_core::ota::decide_rollout`
- The behavioural stress models

## Depends on / used by

- **Depends on** (workspace crates): `pos-core`.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- `src/main.rs`: the report `just simulate` prints
- `src/capacity.rs`: the capacity model and `reconcile`
- `src/fleet.rs`: fleet rollout scenarios
- `src/stress.rs`: drain, backpressure and reconciliation models

## Rules

- [ADR-0048](../../docs/adr/0048-ota-rollout-model.md)

## How to test

```bash
cargo test -p pos-simulator
just simulate
```
