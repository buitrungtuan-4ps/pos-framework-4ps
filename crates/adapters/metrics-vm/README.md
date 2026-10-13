# `metrics-vm`

**Layer** `adapter` · **Owners** @maintainers-cloud

## What it is

The cloud's `MetricsSink` over VictoriaMetrics. `record` never waits on the network: a sample goes into a bounded queue and a background task flushes batches. A full queue drops the batch, so slow monitoring never becomes a trading outage.

## Owns

- `MetricsSink` with a bounded queue
- The VictoriaMetrics JSON line import

## Depends on / used by

- **Depends on** (workspace crates): `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`.
- **Used by** (workspace crates): `pos-cloud`.

## Entry points

- `src/sink.rs`: `VmMetrics`, `QUEUE_CAPACITY`, `MetricTransport`

## Rules

- [ADR-0026](../../../docs/adr/0026-port-shapes.md) · [ADR-0031](../../../docs/adr/0031-cloud-adapter-transports.md)

## How to test

```bash
cargo test -p metrics-vm
```
