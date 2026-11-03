# ADR-0193 — Conditions and reactions are written in CEL

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Adds a dependency
(`cel`) · Relates to [ADR-0007](0007-in-house-vs-dependency.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

Flows need conditions ("only for dine-in", "only above 500,000 ₫", "only if the item is alcohol"), and
the owner wants reactions: "when … if … then …". The same condition must give the same answer on the
till, which paints at once, and on the edge, which re-checks
([ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)).

The owner chose CEL, not Rhai. Rhai's limits are off by default and it can import modules unless
built without them; a general scripting language is code and must be reviewed as code. CEL is not
Turing-complete and evaluates in time linear in the expression. The repository has no expression
language today.

## Options considered

1. **JsonLogic** (`datalogic-rs`). Safe, but unreadable to the people who author it, and the console
   would have to hide it completely.
2. **Two evaluators**, Rust on the edge and TypeScript in the till. Fast in the till, but two
   implementations of one language disagree at the edges (integer overflow, missing fields).
3. **One evaluator, the `cel` crate, natively on the edge and in the cloud, compiled to WebAssembly
   in the till** (chosen).

## Decision

1. **The dependency is `cel`** (crates.io `cel` 0.15, MIT, `cel-rust`). It is the maintained Rust
   implementation of the CEL specification, with no I/O and no runtime of its own. It enters
   `cargo-deny` review in the pull request that adds it, and lives in one new pure crate,
   `pos-rules`, which `pos-core` does not depend on. `pos-rules` follows `pos-core`'s rules: no
   `unwrap`, `expect`, `panic!` or `unsafe`, and any panic inside the crate is caught at its boundary
   and reported as a failed evaluation.
2. **One evaluator.** The cloud uses `pos-rules` to validate at publish, the edge to re-check, and the
   till runs the same crate compiled to `wasm32`, loaded once at boot, never on a tap. If the
   phase 0 spike shows that the crate cannot build for `wasm32`, or that its module is over 400 kB
   gzipped, this record is amended before any code.
3. **A restricted dialect.**
   - **No loop macros:** `all`, `exists`, `exists_one`, `map` and `filter` are refused. `has()` stays.
   - **Limits:** at most 1,024 characters, 256 nodes and a depth of 32 per expression; refused at
     publish beyond them.
   - **Integers only for money.** Amounts are exposed in minor units as `int`; a `double` literal or
     a `double` in a comparison with an amount is refused.
   - **Functions** are an allow-list: comparison, boolean logic, integer arithmetic, `in`, `size`,
     `contains`, `startsWith`, `endsWith`. No time library: the time slot and the local hour and
     weekday are variables, read from `ClockSource`.
4. **What a condition can see.** Read-only views: `order`, `line`, `bill` (amounts in minor units),
   `channel`, `device` (its class), `role` (the signed-in person's role, never their name or id),
   `now`, and custom fields not marked personal (ADR-0194). Personal fields are not visible to rules,
   so a rule can never copy personal data into a log or a refusal.
5. **Reactions: when, if, then.** A package reaction names:
   - **when:** a trigger from a fixed list (a screen entered, a step completed, a line added, an order
     opened, a bill opened);
   - **if:** a condition;
   - **then:** one catalogue action with fixed parameters
     ([ADR-0191](0191-every-act-is-in-a-typed-catalogue.md)).
   Only `ACTION_EFFECT_UI` and `ACTION_EFFECT_EDGE_WRITE` actions may be a reaction's *then*. A money
   or external action can only be offered to a person as a step. A reaction does not trigger further
   reactions, and one trigger runs at most 16. Reactions run on the till, under the signed-in
   person's permissions; the edge checks the resulting command as it checks any other.
6. **A condition is a value in the package.** It is compiled and validated in the cloud at publish,
   compiled again on the edge at apply (last-known-good if it fails), and cached compiled in both.

## Consequences accepted

- **A new third-party dependency** in a new crate, reviewed under `cargo-deny`, and a `wasm32` build
  of that crate in the till's build.
- **The till loads a WebAssembly module at boot.** Its size counts toward the till's bundle budget,
  and its evaluation time toward each act's 10 ms ([ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)).
- **What CEL cannot express needs a function** (ADR-0199) or a release. The dialect stays small on
  purpose.
- **The console writes CEL for people** (phase 4); a person can read it, but need not type it.
