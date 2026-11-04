# `pos-rules`

**Layer** `core` · **Owners** @maintainers-architecture

## What it is

The evaluator for a package's conditions, in a restricted dialect of CEL. The cloud compiles a condition to validate a package before publish, the edge compiles it again when it applies the package and evaluates flow guards, and the till runs this crate compiled to `wasm32`, built once at boot. Pure: no I/O, no clock, no `unwrap`, `expect`, `panic!` or `unsafe`.

## Owns

- The dialect: no loop macros, an allow-list of functions, at most 1,024 characters, 256 nodes and a depth of 32, no fractions
- The views a condition reads (`order`, `line`, `bill`, `channel`, `device`, `role`, `now`) and the integer-only facts they hold

## Depends on / used by

- **Depends on** (workspace crates): no workspace crate.
- **Used by** (workspace crates): `pos-edge-app`.

## Entry points

- `src/lib.rs`: `Rules`, `Condition`, `RuleError`
- `src/dialect.rs`: the limits, `FUNCTIONS`, `LOOP_MACROS`
- `src/facts.rs`: `Facts`, `Fact`, `Variable`

## Rules

- [ADR-0193](../../docs/adr/0193-conditions-and-reactions-are-written-in-cel.md)

## How to test

```bash
cargo test -p pos-rules
```
