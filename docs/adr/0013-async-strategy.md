# ADR-0013 — Sans-I/O domain core, async ports, static dispatch

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** Three requirements collide. Adapters are inherently asynchronous — sqlx, NATS,
reqwest and axum are all async-native. `pos-core` and `pos-ports` may depend only on `std`,
`serde` and pure computation crates, and a CI test enforces that allow-list
([ADR-0021](0021-corrected-port-list.md), formerly 0006). And `pos-core` tests must run in
milliseconds with in-memory fakes.

Rust's native `async fn` in traits needs no dependency but is not `dyn`-compatible, so it
cannot be used behind a trait object. `async_trait` solves that but is a procedural macro
that boxes every future — a dependency inside `pos-ports`, which is the one place the
allow-list exists to protect.

**Options considered.**

1. `async_trait` on every port. Simple and familiar, and it gives trait objects. Rejected:
   it puts a proc-macro dependency and a per-call allocation inside the crate whose whole
   purpose is to have neither, and it would make the dependency-rule test a negotiation
   rather than a law.
2. Synchronous ports, with adapters wrapping their I/O in `spawn_blocking`. Rejected: it
   contradicts the standing "no blocking in async" rule, and it fights libraries that are
   async all the way down. `spawn_blocking` is the right tool for SQLite, not for NATS.
3. Async ports with `dyn` dispatch and native `async fn`. Not possible as stated — that
   combination does not compile.
4. **Split the ports by who calls them.** Chosen.

**Decision.**

*`pos-core` is sans-I/O.* It is entirely synchronous and performs no I/O of any kind. Its
shape is `decide(state, command, context) -> Result<Vec<Effect>, DomainError>`: it receives
state that has already been loaded, and returns the events and side-effect requests that
should follow. It never awaits anything, so it needs no executor and no runtime.

*Two sync ports.* `ClockSource` and `IdGenerator` are the only ports `pos-core` holds. Both
are plain `fn`, both are trivially fakeable, and both remain `dyn`-compatible. These two
exist as ports precisely so tests can drive a shift across midnight or a promotion expiring
mid-bill.

*Every other port is async*, declared with native `async fn` in trait — no macro, no
dependency, no boxing. Only the binaries and the adapters call them.

*Dispatch is static.* Each binary knows its adapters at compile time and wires them
generically. Where a family genuinely has several implementations compiled in at once —
`DeliveryVendor`, `PaymentTerminal`, `ShippingDispatch`, `Fiscalization` — the binary holds
an enum whose variants are those adapters and dispatches with a `match`. This is not a
workaround: [`design-principles.md`](../design-principles.md) states that a plain `match`
inside a binary beats a plugin system until the third case arrives.

**Consequences.**

- `pos-ports` stays inside the allow-list. No procedural macro, no allocation per call, no
  runtime, and the dependency-rule test keeps its teeth.
- `pos-core`'s test suite needs no executor. That, and not merely the use of fakes, is what
  makes it run in milliseconds — the tests are ordinary synchronous functions.
- **`TxContext` becomes enforceable by shape rather than by review.** Because the domain
  returns effects instead of performing writes, "an event written outside a transaction" is
  not a thing the domain can express. The only API that consumes a `Vec<Effect>` takes
  `&mut TxContext`, so the event and its transaction commit together or not at all.
- Cost: adding a member to an adapter family means editing an enum in the binary. That is a
  deliberate, reviewable edit rather than a plugin registration, and it is the same trade
  the design principles already accept.
- Cost: each binary needs a thin application layer — load, decide, apply — written per use
  case. This is not free indirection; it is the layer that owns the transaction, so it has
  to exist somewhere regardless.
- If a port ever genuinely needs a trait object across an async boundary, the escape hatch
  is a boxed wrapper **in the binary**, never in `pos-ports`.
