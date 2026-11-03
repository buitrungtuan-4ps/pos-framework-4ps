# ADR-0199 — A function is signed, pure WebAssembly that returns a decision

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0083](0083-integration-doctrine.md) ("not a plugin runtime") · Adds a dependency (`wasmtime`) ·
Relates to [ADR-0047](0047-minisign-verification.md), [ADR-0048](0048-ota-rollout-model.md),
[ADR-0092](0092-artifact-trust-chain.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

CEL ([ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md)) answers conditions. Some cases
will need logic it cannot express: a loyalty scheme's eligibility over a basket, a routing rule over
many stations. The owner decided custom code runs as WebAssembly functions, in the style of Shopify
Functions: pure, a typed input to a decision, fuel and memory limits, signed, rolled out by ring,
and only when a real case needs it.

[ADR-0083](0083-integration-doctrine.md) says the edge is "not a plugin runtime": adapters are
compiled Rust crates, and no binary runs untrusted code. Releases are already signed by CI with
minisign, verified by the edge against keys baked into the build, and never signed on the VPS
([ADR-0047](0047-minisign-verification.md), [ADR-0092](0092-artifact-trust-chain.md), roadmap-v3 D1);
they roll out by canary ring with self-test and rollback ([ADR-0048](0048-ota-rollout-model.md)).

## Options considered

1. **Rhai or Luau scripts in the package.** The owner declined Rhai; any script in a package is code
   authored in a console and run on every till without review.
2. **No custom code; every case a release.** Safe, slow, and the reason a chain outgrows a POS.
3. **Reviewed functions, built and signed like releases, run by `wasmtime` in a sandbox with no
   imports** (chosen).

## Decision

1. **Built only for a real case.** Nothing of this ships until the owner approves the first case
   that CEL and the catalogue cannot meet (`docs/design-principles.md`, YAGNI). This record fixes the
   shape so that case does not need a new decision.
2. **A function implements a function kind.** A kind is a catalogue entry like an action: its
   `function_kind_id`, a typed input, a typed decision, and a **default decision** used whenever the
   function fails. A decision chooses among things already configured: allow or refuse with a
   reason key, which configured promotion applies, which station. **A decision is never an amount,
   and a function cannot write an event, move money or call out**: it has no import but its memory.
3. **Pure.** The input is everything it may see, including `now`; no clock, no randomness, no I/O,
   no WASI. The same input gives the same decision, so a function is tested by input and output
   vectors that ship with it.
4. **Limits**, enforced by the runtime:
   - fuel: 5,000,000 units per call; memory: 16 MiB; module: at most 1 MiB;
   - wall clock: 5 ms per call through epoch interruption;
   - on any trap, exhaustion or malformed output, the kind's default decision applies, and the
     failure is counted and reported in the heartbeat.
5. **Code is reviewed, built and signed like a release.** A function's source lives in the
   repository (`functions/<name>/`, Rust compiled to `wasm32-unknown-unknown`), goes through a pull
   request, and is built and signed by CI with a function-signing key, never on the VPS. The edge
   trusts only the function keys baked into its build, verifies with the existing minisign verifier,
   and refuses an unsigned or unknown-key module. The cloud only hosts it.
6. **Rolled out by ring.** A package version names a function by id and version. The cloud delivers
   a new function version by ADR-0048's rings: canary stores first, then the rest. Before activating
   it, the edge runs its shipped vectors; a failure keeps the previous version and is reported.
7. **On the edge only.** `wasmtime` runs in the edge, in the command path or for a step the till asks
   about. A function's answer is an edge answer, so the till paints it pending first
   ([ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)).
8. **The dependency is `wasmtime`** (Bytecode Alliance, Apache-2.0 WITH LLVM-exception, already in
   `deny.toml`'s allow-list), pinned to one release (50.x at the time of writing), with only the
   Cranelift compiler and none of the WASI features. It is chosen for fuel and epoch interruption,
   its security process and its release cadence. It enters `cargo-deny` review in the pull request
   that adds it, and only `pos-edge` depends on it, through an adapter crate.

## Consequences accepted

- **ADR-0083 is amended**: the edge runs code it did not compile, but only code this repository
  reviewed and CI signed. A function from anywhere else cannot run.
- **A large dependency** with its own transitive tree, and a few megabytes of edge binary, accepted
  when the first case lands, not before.
- **A function cannot help with money directly.** It can only pick among configured options; a new
  money rule is still core work with its own ADR.
- **HQ cannot write a function in the console.** It asks for one, and engineers write it. That is the
  price of review and signing.
