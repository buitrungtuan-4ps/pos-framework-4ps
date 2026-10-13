# `minimal-edge`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

Boots `pos_edge` over the in-memory fakes with a fixed development store id and no configuration file. It is what `just run-edge` runs, and the store the till's replay tests drive in CI.

## Owns

- A runnable store from the repository alone

## Depends on / used by

- **Depends on** (workspace crates): `pos-edge`, `pos-fakes`, `pos-proto`.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- `src/main.rs`: the whole example

## Rules

- [ADR-0109](../../docs/adr/0109-counting-the-taps-an-operator-makes.md)

## How to test

```bash
cargo build -p minimal-edge
just run-edge
```
