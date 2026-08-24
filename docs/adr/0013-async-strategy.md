# ADR-0013 — Sans-I/O domain core, async ports, static dispatch

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18
**Amended by** [ADR-0026](0026-port-shapes.md) — three ports do not follow the async rule stated here
**Amended by** [ADR-0027](0027-country-modules.md) — the runtime-registry rule below applies to vendor families, not to countries

**Context.** Three requirements collide. Adapters are inherently asynchronous — sqlx, NATS,
reqwest and axum are all async-native. `pos-core` and `pos-ports` may depend only on `std`,
`serde` and pure computation crates, and a CI test enforces that allow-list
([ADR-0021](0021-corrected-port-list.md)). And `pos-core` tests must run in milliseconds with
in-memory fakes.

Rust's native `async fn` in traits needs no dependency but is not `dyn`-compatible, so it
cannot be used behind a trait object. `async_trait` solves that but is a procedural macro
that boxes every future — a dependency inside the crate the allow-list exists to protect.

There is a second, less obvious problem. `pos-core` genuinely needs two ports:
`ClockSource` and `IdGenerator`. The apparently natural arrangement — `pos-core` depends on
`pos-ports`, which keeps its pure traits in one module and its I/O traits in another behind a
feature — **does not work.** Cargo unifies features across the whole build graph, so the
moment any binary enables the I/O feature, those traits are compiled into the graph and are
nameable from `pos-core`. Feature gating would give a documentation guarantee, not a
compile-time one, and the only enforcement left would be a grep — exactly the class of rule
this project exists to eliminate.

**Options considered.**

1. `async_trait` on every port. Familiar, and it gives trait objects. Rejected: a
   proc-macro dependency and a per-call allocation inside the crate that must have neither,
   and it would make the dependency-rule test a negotiation rather than a law.
2. Synchronous ports with adapters wrapping I/O in `spawn_blocking`. Rejected: it burns a
   thread per in-flight payment terminal and makes cancellation — a customer walking away
   mid-tap — unrepresentable.
3. Async ports with `dyn` dispatch and native `async fn`. Does not compile.
4. One `pos-ports` crate with pure and I/O traits behind features, depended on by
   `pos-core`. Rejected for the feature-unification reason above.
5. **Determinism traits in `pos-proto`; `pos-core` and `pos-ports` as siblings with no edge
   between them.** Chosen.

**Decision.**

*`pos-core` does not depend on `pos-ports` at all.* They are siblings over `pos-proto`:

```
                pos-proto        wire types, value types, determinism traits
               /         \
        pos-core        pos-ports
               \         /    \
             pos-fakes    pos-contract-tests
                    \        /
                  adapters, binaries
```

`ClockSource` and `IdGenerator` live in `pos_proto::determinism`, because they are total
functions over `pos-proto`'s own value types and involve no I/O. `pos-ports` re-exports them,
so the sixteen-port list keeps exactly one definition of each. The consequence is the point:
**"`pos-core` cannot perform I/O" becomes a property of the dependency graph**, checkable
with `cargo tree -p pos-core`, not a lint that a reviewer has to enforce.

*`pos-core` is sans-I/O and synchronous.* Its shape is
`decide(state, command, ctx) -> Result<Decision, DomainError>`, where `Decision` carries the
events to append, the post-commit effects, and the next state. It performs no I/O and awaits
nothing, so it needs no executor.

*The clock is passed as a value, not as a trait object.* `DecisionCtx` carries
`now: Timestamp` and `business_date: BusinessDate`, read once by the caller. `pos-core`
therefore cannot read the clock twice inside one decision and get two answers, which removes
a whole class of nondeterminism and makes replay exact. `IdGenerator` stays a trait because
the number of identifiers needed is data-dependent — splitting a bill into *k* parts.

*Every other port is async*, declared with native `async fn` in trait: no macro, no
dependency, no allocation on the happy path.

*Dispatch is static by default.* Where a family genuinely needs runtime selection —
`DeliveryVendor`, `PaymentTerminal`, `PrinterDriver`, `Fiscalization` — `pos-ports` carries a
hand-written object-safe mirror (`DynDeliveryVendor`, returning
`Pin<Box<dyn Future + Send>>`) with a blanket impl bridging from the ergonomic trait.
Adapters implement only the plain `async fn` version. The cost is one `Box::pin` on a path
that is about to make an HTTP request; the benefit is no proc macro in `pos-ports`.

**Consequences.**

- `pos-ports` stays inside the allow-list, and the no-I/O rule on `pos-core` is enforced by
  the graph rather than by discipline.
- `pos-core`'s tests need no executor at all. That, more than the use of fakes, is what makes
  the suite run in milliseconds — the tests are ordinary synchronous functions.
- **`TxContext` becomes enforceable by shape.** Because the domain returns effects rather
  than performing writes, "an event written outside a transaction" is not expressible: the
  only API consuming a `Decision` takes a transaction handle. `Decision` is `#[must_use]`, so
  a decision that is never applied is a warning.
- Two support crates are needed and are deliberately **not** subject to the allow-list:
  `pos-contract-tests` (the shared suites, which need an executor) and `pos-fakes` (in-memory
  implementations, a dev-dependency of `pos-core`, which runs the suites against itself).
- Cost: adding a member to an adapter family means editing an enum or registry in the binary.
  A deliberate, reviewable edit rather than a plugin registration.
- Cost: each binary needs a thin application layer — load, decide, apply. It is the layer that
  owns the transaction, so it has to exist somewhere regardless.
