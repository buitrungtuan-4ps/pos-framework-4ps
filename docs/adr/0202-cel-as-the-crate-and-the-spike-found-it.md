# ADR-0202 — CEL as the crate and the spike found it

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-03
· Amends [ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md) decisions 1, 2 and 3
· Relates to [ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Proposed: it waits on the owner's acceptance of what phase 0's spike (`spike/cel-on-both-sides`,
`spikes/cel/FINDINGS.md`) and the `pos-rules` crate found about ADR-0193's assumptions.

## Context

ADR-0193 chose the `cel` crate for one evaluator on the cloud, the edge and the till, with a
restricted dialect, and asked for an amendment if the crate could not build for `wasm32` or came
to more than 400 kB gzipped. It does build, and it is within that limit. Building `pos-rules` found
five things the record assumed or did not say:

- **The module's size.** At `opt-level=3` with LTO, the wasm is about 1.0 MB raw and **293 kB
  gzipped**; at `opt-level=z`, 237 kB, with evaluation 1.5 to 2 times slower. Today's whole till
  bundle is 111 kB gzipped. Instantiating takes 6 ms (9 ms at 4× throttle) and compiling five
  guards 27 ms (109 ms at 4×). Evaluating one is under 10 µs even at 4×.
- **The crate rejects nothing at compile time.** Without the macros, `items.all(i, …)` still
  parses as a plain call and fails only when it runs. The crate has no type checker, and it has
  `double`s.
- **The parser's stack.** Its ANTLR recursion overflows a 2 MiB thread stack at about 16 nested
  brackets in an unoptimised build. A release build reaches its own limit of 96 without trouble.
  An overflow aborts the process; it cannot be caught.
- **Release builds abort on a panic.** The workspace's release profile is `panic = "abort"`, so
  "a panic is caught at the crate's boundary" (decision 1) holds only in tests and debug builds.
- **A guard that cannot be answered.** A missing field or an overflow is an evaluation error, and
  ADR-0192 did not say what a guard does with one.

## Decision

1. **The module is loaded at boot, never in a tap.** About 293 kB gzipped counts toward the till's
   bundle. The till builds the evaluator and compiles a package's conditions at boot and when a
   package version is applied, and evaluates only compiled conditions on a tap. Whether to ship the
   `opt-level=z` build is decided with the bundle budget in phase 2, by measurement.
2. **The dialect is enforced at compile time by allow-lists** (`pos-rules`, `Rules::compile`):
   - the macro list is empty except `has()`, and `all`, `exists`, `exists_one`, `map` and
     `filter` are refused by name;
   - any function off the list ADR-0193 decision 3 names (comparison, boolean logic, integer
     arithmetic, `in`, indexing, `size`, `contains`, `startsWith`, `endsWith`) is refused, and so
     are struct literals and any name but the declared views;
   - **the money lint:** a `double` literal and `double()` are refused outright, and a fact handed
     to a condition has no fractional type, so an amount is always an integer in minor units.
3. **Brackets are counted before parsing.** Nesting deeper than 32 outside string literals is
   refused from the text, so the parser never recurses past the depth limit. A debug build can
   still overflow between about 16 and 32; that is accepted, because only development builds run
   unoptimised, and a test that parses deep conditions runs on a larger stack.
4. **The limits are the defence, not `catch_unwind`.** The crate keeps catching a panic where the
   build unwinds, and a release build aborts on one. The length, node, depth and bracket limits,
   and the crate's own errors for overflow, division by zero and a missing field, are what keep a
   condition from taking the process down.
5. **A guard fails closed.** On the edge, a guard's `applies_when` that cannot be answered applies,
   and a `done_when` that cannot be answered is not done, so the command is refused with
   `FLOW_PREREQUISITE_UNMET`, naming the step, and a manager can fix the guard. Publish validation
   is expected to catch a guard that reads a field it cannot have.

## Consequences accepted

- The till's first download roughly triples, once, and is cached after that.
- A condition the dialect refuses is refused before publish, so the console has to say why;
  `RuleError` carries the reason.
- A badly written guard stops an act until it is fixed, rather than letting the act through.
- A development build can crash on a condition with 16 to 32 nested brackets, which no production
  build does.
