# `pos-till-rules`

**Layer** `adapter` · **Owners** @maintainers-architecture

## What it is

The till's rules module: `pos-rules` and `pos-core`'s flow engine, built for `wasm32-unknown-unknown` and loaded by the till once at boot, never on a tap. The till asks it, in JSON, which steps of a flow it may offer and which prerequisite a step is missing, over conditions compiled once when the package's flows are loaded. It is the same engine the edge re-checks with, so the till and the edge cannot disagree on what a condition means.

## Owns

- The boundary: three exported functions (`till_rules_input`, `till_rules_call`, `till_rules_output`) and the JSON requests `load`, `available`, `missing` and `holds`
- The facts the till hands a condition, as JSON: whole numbers only, no nulls, nested at most eight deep
- The request size cap, `REQUEST_BYTES_MAX` (1 MiB)

## Depends on / used by

- **Depends on** (workspace crates): `pos-core`, `pos-proto`, `pos-rules`.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- `src/engine.rs`: `Engine::answer`, the requests and their answers, with no `unsafe`
- `src/abi.rs`: the three exported functions, the only `unsafe` (exporting a name)

## Rules

- [ADR-0192](../../docs/adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)
- [ADR-0193](../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md)
- [ADR-0202](../../docs/adr/0202-cel-as-the-crate-and-the-spike-found-it.md)
- Every error is an answer, `{"ok":false,"error":…}`; no request may panic, because the module aborts on one

## How to test

```bash
cargo test -p pos-till-rules
cargo build -p pos-till-rules --target wasm32-unknown-unknown --profile till-rules
```
