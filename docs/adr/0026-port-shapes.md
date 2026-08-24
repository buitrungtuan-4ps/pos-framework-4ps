# ADR-0026 — Port shapes: one failure type, one transaction handle, and three corrections

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18
**Amends** [ADR-0013](0013-async-strategy.md) · **Depends on** [ADR-0021](0021-corrected-port-list.md)

**Context.** [ADR-0021](0021-corrected-port-list.md) fixed *which* sixteen ports exist and
[ADR-0013](0013-async-strategy.md) fixed *how* they dispatch. Neither says what a port method
returns when it fails, how a caller proves it holds a transaction, or what the contract suites
may ask an adapter to do. Writing the traits surfaced four questions that have to be answered
once, centrally, or they get answered sixteen times, differently.

---

## 1. One failure type for all ports

**Options.** A per-port error enum; `Box<dyn Error>`; one shared `PortError`.

Per-port enums are the textbook answer and they are wrong here. The framework's job at a port
boundary is nearly always the same: decide whether to retry, whether to park the work in an
error mailbox, and what to tell the operator. Fifteen enums would each need that same
classification bolted on, and the *first* place they get unified is the application layer —
which is exactly where a `From` impl per port would then live. `Box<dyn Error>` loses the
classification altogether.

**Decision.** One `PortError`, carrying:

- an [AIP-193](../naming-and-api.md) `ErrorStatus` reused from `pos-proto` — the same
  vocabulary the HTTP surface already speaks, so an adapter failure maps to a response without
  a translation table;
- the `PortName` that produced it, as an enum of the sixteen, so metrics and the error mailbox
  can partition by port without string matching;
- a message, and an optional `source` as `Box<dyn Error + Send + Sync>` — `std` only, no
  dependency.

`is_retryable()` lives on the status, not on the call site. `Unavailable` and
`ResourceExhausted` are retryable; `InvalidArgument`, `NotFound` and `FailedPrecondition` are
not, and retrying them is the bug that turns a bad request into an outage. The classification
already exists on `ErrorStatus` and is not re-stated here, so the port boundary and the HTTP
boundary cannot drift apart.

**Consequence.** `RESOURCE_EXHAUSTED` becomes the framework's standard back-pressure signal.
A bounded queue that is full returns it rather than growing, blocking, or dropping — which is
what makes `capacity-and-reliability.md`'s "unsent events wait in the outbox" a mechanism
rather than a hope.

## 2. `Transactional` is a separate port, so the compiler enforces one transaction

The requirement is stronger than it looks. `glossary.md` says the outbox row is written *in
the same transaction as the state change*, and `pos-spec.md` §5 requires the receipt number to
be allocated inside the bill transaction. So one transaction must span several operations, and
at the edge those operations span two ports — `EventStore` and `ConfigStore` are both
implemented by `store-sqlite`.

**Options.**

1. `append(&self, events)` with an implicit ambient transaction. Rejected: this is precisely
   the "reviewable but not enforceable" rule the roadmap says to eliminate.
2. `append(&self, tx: &mut dyn TxContext, ...)`. Rejected: a handle from one adapter could be
   passed to another, and it compiles.
3. **A `Transactional` supertrait with an associated `Tx` type.** Chosen.

```rust
pub trait Transactional {
    type Tx: TxContext;
    async fn begin(&self) -> Result<Self::Tx, PortError>;
}
pub trait EventStore: Transactional { … }
pub trait ConfigStore: Transactional { … }
```

Because `EventStore` and `ConfigStore` both *require* `Transactional` rather than each
declaring their own handle, an adapter implementing both has exactly one `Tx` type — so
"append the event and allocate the receipt number in one transaction" is not a convention the
adapter honours, it is the only thing that type-checks. Passing `store-postgres`'s handle to
`store-sqlite` is a type error.

**`Tx` is owned, not borrowed from `&self`.** A generic associated lifetime would be more
faithful to how `sqlx` models a transaction, but it fights the single-writer-thread design
that ADR-0015 is heading towards, where the handle is a channel to
another thread and borrows nothing. Owned costs an `Arc` clone and buys signatures that a
reader can hold in their head.

`commit` and `rollback` take `self` by value, so a committed transaction cannot be reused.
Dropping without committing rolls back — the behaviour SQLite and PostgreSQL already have,
stated here so an adapter cannot reasonably choose otherwise.

## 3. The outbox cursor is commit order, not identifier order

Tempting and wrong: page the outbox by `event_id`, since ULIDs sort by time and the event feed
already pages by `page_token=<ulid>`.

The failure is a lost write. Suppose transaction 1 commits events *A* and *C* while
transaction 2 is still open with *B*, where `A < B < C`. A reader that acknowledges "through
*C*" has skipped *B*, and *B* is not late — it is gone, because the acknowledgement is a
high-water mark. The store server is a single writer today, which hides the bug; the first
adapter with concurrent writers finds it in production, silently, as missing revenue.

**Decision.** The outbox is ordered by an adapter-assigned position taken **at commit**, and
`OutboxPosition` is opaque. Acknowledgement is a high-water mark and therefore idempotent:
acknowledging the same position twice removes nothing extra. The contract suite pins both
properties.

This is the answer to one of `docs/roadmap.md` D10's open items — the outbox *cursor and ack
protocol*. The table structure remains P4's.

## 4. Publication cannot be transactional

`MessageLink` has no `Tx`. It cannot: NATS is a separate system, and no two-phase commit
exists between it and SQLite. Any design that appears to publish transactionally is either
losing events on a crash between commit and publish, or double-publishing them.

That is the whole reason the outbox exists, and it fixes the delivery guarantee at
**at-least-once**: publish, then acknowledge, and a crash in between replays. Consumers are
idempotent by ULID — which `architecture.md` §6.2 already requires of webhook receivers, and
which the `EventStore` contract suite requires of every store.

## 5. Three corrections to ADR-0013

ADR-0013 says "every other port is async". Writing them found three cases where that is
either false or misleading.

**`Signer` is synchronous.** Verifying a minisign signature is arithmetic. Worse, it runs
during OTA verification at startup — potentially *before* an async runtime exists — so an
`async fn` would force a runtime into the one code path that most needs to work when
everything else is broken. `KeyVault` stays async: DPAPI, a TPM and a keyring are syscalls
that block. So the count is **three synchronous ports and thirteen asynchronous**, not two and
fourteen.

**`OrderIn` points the other way.** The other fifteen are driven ports: the framework calls
out. `OrderIn` is a *driving* port — the application implements it, and `vendor-grab`,
`POST /v1/orders` and the QR module call in. Its contract suite therefore tests the
*framework's* implementation, not a vendor's, and its idempotency key is the caller's
`external_reference` rather than a ULID we minted. This is why
[ADR-0012](0012-qr-ordering-via-cloud.md) can say QR ordering is architecturally almost free:
there is nothing to build but a caller.

**`PrinterDriver` reports its code page; it does not decide about bitmaps.**
`pos-spec.md` §13 requires any line outside the printer's code page to be rendered as a
bitmap. Putting that decision in the adapter would duplicate it per vendor and let two
printers disagree about the same receipt. So the port exposes `capabilities()` — code page,
column count, whether it cuts, whether it kicks a drawer — and the framework decides. Adapters
carry the vendor's protocol and nothing else, which is the rule `architecture.md` §6.1 already
states for vendor adapters.

## 6. Fault injection belongs to the harness, not the port

`EventStore`'s stated contract includes *survival of a crash mid-transaction*. Something has
to cause the crash.

**Decision.** Every suite is parameterised by a **harness trait** that lives in
`pos-contract-tests`, and the destructive operations are declared there:

```rust
pub trait EventStoreHarness {
    type Store: EventStore;
    async fn fresh(&self) -> Result<Self::Store, HarnessError>;
    async fn lose_power(&self, store: Self::Store) -> Result<Self::Store, HarnessError>;
}
```

The alternative — a `simulate_crash` method on `EventStore` — would ship a
"corrupt yourself now" entry point in every production adapter, reachable from anywhere that
holds the trait. A harness is test-only by construction.

**Suites are emitted by a macro, so `pos-contract-tests` needs no executor.** Each adapter
invokes `event_store_suite!(harness_expr, block_on_fn)` and supplies its own runtime — tokio
for `store-sqlite`, and for `pos-fakes` a twenty-line poller that drives a future exactly one
poll. That poller failing means a fake yielded, which the fakes must never do; it is the
mechanism behind "the domain suite runs in milliseconds" and it costs no dependency in either
crate.

---

**Consequences.**

- Sixteen ports, one failure vocabulary, one back-pressure signal, one transaction concept.
- Two support crates outside the dependency allow-list, exactly as ADR-0013 anticipated:
  `pos-contract-tests` and `pos-fakes`. Neither depends on a runtime.
- The suites are the deliverable that makes *swappable* verified rather than claimed, so a
  port added later without a suite is an incomplete port, and CI counts them.
- Cost: `PortError` is a shared type, so widening it touches every adapter. Accepted — the
  classification it carries is the thing every adapter would otherwise reinvent.
