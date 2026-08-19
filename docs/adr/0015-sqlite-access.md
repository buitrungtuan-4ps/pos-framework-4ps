# ADR-0015 — SQLite access at the edge: `rusqlite` behind a single-writer thread

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Depends on** [ADR-0013](0013-async-strategy.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0007](0007-in-house-vs-dependency.md)

**Context.** The store server keeps its truth in one SQLite database in WAL mode
([`architecture.md`](../architecture.md) §4). Two facts about SQLite shape every access decision.
SQLite is **synchronous** — its C API blocks the calling thread — while `pos-ports`' `EventStore` and
`ConfigStore` are **async** ([ADR-0013](0013-async-strategy.md)), so something has to bridge the two.
And SQLite permits **exactly one writer at a time**; a second writer gets `SQLITE_BUSY`, and
`busy_timeout` only turns that into a wait. A store server has many concurrent readers — every screen,
every device on the LAN — but its writes (a fired line, a settled bill, an outbox row) must serialise.

[ADR-0026](0026-port-shapes.md) already requires that the outbox position be assigned **at commit** and
be strictly increasing with no gap a reader could skip. That is only cheap to guarantee if writes go
through a single point that commits them in order.

[ADR-0007](0007-in-house-vs-dependency.md) settles the build-vs-buy question outright: SQLite is on the
"not worth writing" list — "bugs there do not show up in tests." So this ADR is only about which Rust
binding, and how it meets the async port surface.

**Options considered.**

1. **`sqlx` with its SQLite backend.** Async-native, compile-time-checked queries. Rejected on three
   counts. Its SQLite driver still calls the blocking C library on a background thread — it does not make
   SQLite concurrent, it hides a thread pool — so it buys an async *feel* over a database that is
   single-writer regardless. Compile-time query verification (`sqlx::query!`) needs a live database or a
   committed offline cache at build time, which puts a database in the CI critical path for a store that
   has no schema drift a migration test would not already catch. And a connection *pool* is the wrong
   shape for a single-writer store: pooled writers just contend for the one write lock.
2. **`rusqlite` with a connection per async task**, relying on `busy_timeout` to serialise writes.
   Rejected: it turns write contention into latency and reintroduces exactly the `SQLITE_BUSY` retry
   loops the single-writer model exists to remove, and it makes "assign the outbox position at commit" a
   cross-task problem rather than a local one.
3. **`rusqlite` behind one dedicated single-writer thread.** Chosen.

**Decision.** Use **`rusqlite`** (a thin, widely-used binding to the SQLite C library, with the bundled
`libsqlite3-sys` so the edge depends on no system SQLite on either Windows or Linux) and own the
concurrency ourselves with **one dedicated writer thread**:

- All **writes** go to a single OS thread that owns the one writable `Connection`. The async
  `EventStore`/`ConfigStore` methods send a request over a bounded channel and `await` a oneshot reply,
  so the port surface stays `async fn` exactly as [ADR-0013](0013-async-strategy.md) requires while the
  blocking C calls happen off the async executor. Because every write is serialised through this one
  thread, `SQLITE_BUSY` on write cannot occur, and the outbox position
  ([ADR-0026](0026-port-shapes.md)) is assigned as a monotonic counter **inside the same transaction
  that writes the event**, starting at one so it never collides with `OutboxPosition::START`, with no
  cross-task coordination.
- **Reads** may use a small set of read-only connections (WAL lets readers run concurrently with the
  single writer without blocking it), opened with `PRAGMA query_only = true` so a read path physically
  cannot write.
- `TxContext` ([ADR-0026](0026-port-shapes.md)) buffers a transaction's pending events and config update
  in memory and flushes them to the writer thread in **one** `BEGIN IMMEDIATE` … `COMMIT` on
  `commit()`; `rollback()` and drop discard the buffer, having written nothing. So "an event written
  outside a transaction" stays unrepresentable — the bridge exposes no write that is not part of a
  transaction handle obtained from `begin()`, and a dropped handle rolls back for free.
- The bounded channel gives natural backpressure: if the writer falls behind, `begin()` returns
  `resource_exhausted` rather than the queue growing without limit. A crash between `BEGIN IMMEDIATE`
  and `COMMIT` loses only the uncommitted transaction — the crash-mid-transaction contract the
  `EventStore` suite asserts.

Connection PRAGMAs are fixed once at open, not per-call: `journal_mode = WAL`, `synchronous = NORMAL`
(durable across an application crash; the OS-crash window is accepted because WAL shipping and the
`.pre-update` copy are the real durability story, [`architecture.md`](../architecture.md) §4),
`busy_timeout` as a backstop for the rare reader-vs-checkpoint contention, and `foreign_keys = ON`.

**Consequences.**

- One writer thread is the entire write-concurrency story: no lock contention, no retry loops, and
  gapless outbox positioning falls out for free.
- `rusqlite` is an adapter dependency, not a backbone one, so it sits outside the
  `pos-core`/`pos-ports`/`pos-proto` allow-list and `cargo xtask deps-rule` does not police it; its MIT
  licence and that of `libsqlite3-sys` are already inside `deny.toml`'s allowed set, and the bundled
  build pulls in no `openssl-sys`.
- The bridge is a small amount of hand-written channel plumbing rather than a framework — the cost of
  keeping the port surface async over a synchronous, single-writer database, paid once in `store-sqlite`.
- The fake and the real adapter are exercised by the *identical* `EventStore`/`ConfigStore` contract
  suites, including crash-mid-transaction, because both present the same async, single-writer shape.
- Cloud PostgreSQL access is a separate decision (ADR-0016, P7); PostgreSQL is genuinely concurrent and
  needs a pool, so it does not inherit the single-writer model.
