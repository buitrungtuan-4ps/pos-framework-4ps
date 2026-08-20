# ADR-0031 — Cloud adapter transports: one dependency, two hand-rolled

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0013](0013-async-strategy.md) · [ADR-0016](0016-postgres-access.md) · [ADR-0021](0021-corrected-port-list.md) · [ADR-0026](0026-port-shapes.md)

**Context.** P7 adds the three remaining cloud-side infrastructure adapters behind ports whose
*backends* are already settled and closed to relitigation: `MessageLink` over **NATS JetStream**,
`BlobStore` over **Garage** (S3-compatible), `MetricsSink` over **VictoriaMetrics**. What is *not*
settled is how each adapter talks to its backend — a client library, or hand-written protocol — and
that choice lands real third-party code on the cloud (and, for the link, the edge) dependency
surface. [ADR-0007](0007-in-house-vs-dependency.md) is the governing rule: buy infrastructure that is
genuinely hard and general; build the thin, specific, or soon-deleted thing yourself rather than
carry a heavyweight dependency for it.

The three ports sit very differently against that rule, so one blanket answer would be wrong.

**Decision.**

- **`link-nats` depends on `async-nats`** (the official NATS client), and uses its JetStream API.
  The JetStream client protocol — subject framing, publish acknowledgements, flow control, stream
  and consumer management, reconnection — is exactly the "genuinely hard and general" infrastructure
  ADR-0007 says to buy. Reimplementing it would be a sub-project with its own bugs, and the link is
  load-bearing: it is the store→cloud channel every sale eventually travels. The dependency is worth
  it.

  The handshake is **local**, not a cloud round-trip, which keeps the port "outbound only"
  ([ADR-0021](0021-corrected-port-list.md), `message_link.rs`): `handshake` confirms the broker is
  reachable, ensures this store's JetStream stream exists, and returns the outcome of
  `pos_proto::protocol::negotiate` — the *same* function the fake uses, so the version-overlap rule
  is [ADR-0024](0024-protocol-version-negotiation.md)'s and not a re-statement of it. There is no
  responder to wait on; the cloud consumer validates versions when it reads.

  The **read** counterpart lives in the same crate (`NatsConsumer`, `consumer.rs`), so all JetStream
  wire code is in one adapter. It is a **durable pull consumer**: JetStream tracks the delivery
  position server-side, which is exactly "the cursor over the event log" (`docs/roadmap.md` P7) and
  what a later slice resets to replay. It hands the caller the decoded batch *with* the message
  handles and acknowledges only after the caller has stored it, so with idempotent ingest
  ([ADR-0026](0026-port-shapes.md) §4) the link is at-least-once with exactly-once effect. The one
  thing it discards is a frame that is not a valid envelope — it can never be ingested, so it is
  *terminated* (not left to wedge the cursor) and counted, loudly, never silently. The consume loop
  itself (ack on commit, redeliver otherwise) lives in `pos-cloud` (`cursor.rs`), which owns the
  application it drives; `link-nats` provides only the mechanics.

- **`blob-garage` hand-rolls minimal S3** — SigV4 request signing and HTTP/1.1 over `tokio`, no S3
  SDK. This is the ADR-0007 case in its purest form:
  [ADR-0007](0007-in-house-vs-dependency.md) records that object storage exists in this system
  **only** to satisfy Litestream, and that the port is **deleted outright** once WAL shipping is
  in-house ([ADR-0021](0021-corrected-port-list.md), `blob_store.rs`). Pulling an S3 SDK — dozens of
  transitive crates, its own async runtime assumptions, a large `cargo-deny` surface — for four
  methods over tens-of-megabytes objects that are scheduled for removal is precisely the speculative
  dependency the project refuses. SigV4 for `PUT`/`GET`/`DELETE`/`GET?list-type=2` is a bounded,
  well-specified ~150 lines; its one failure mode (a signature mismatch → `403`) is caught loudly by
  the contract suite running against a real S3 server, so "hand-rolled" here does not mean
  "unverified." Signing uses `hmac` + `sha2`, already in the tree.

- **`metrics-vm` hand-rolls an HTTP POST** to VictoriaMetrics' JSON import endpoint
  (`/api/v1/import`) over `tokio`, behind a **bounded queue drained by a background task**. Telemetry
  sits off the sales path ([ADR-0026](0026-port-shapes.md), `metrics_sink.rs` contract 1), so
  `record` must never block on a slow or dead backend: it enqueues without waiting and the batch is
  dropped when the queue is full. The monitoring profile is sparse by design
  (`docs/capacity-and-reliability.md` turns it off below ~50 stores), the wire is one endpoint, and
  the risk lives in the *buffering*, not the HTTP — so no client library is warranted, and no float
  is used (a sample is an `i64` plus a `MetricUnit`, encoded as an integer value).

**How correctness is proven** (extending [ADR-0026](0026-port-shapes.md) and the pattern
[ADR-0016](0016-postgres-access.md) set for `store-postgres`):

- **Backend semantics under test → a real backend.** `link-nats` and `blob-garage` run the shared
  port contract suites against a real `nats-server -js` and a real S3 server (MinIO/Garage). The
  cursor gets its own real-JetStream suite (`link-nats` `tests/consumer.rs`: read-back, ack advances
  the durable cursor, nak redelivers, poison is terminated) and an end-to-end proof that it drives
  ingest idempotently against a real broker (`pos-cloud` `tests/cursor.rs`, over the fake store).
  These live behind each crate's `integration` Cargo feature, off by default, so the ten-minute
  pull-request gate neither compiles nor runs them; the merge-to-`main` `integration` job runs them
  against pinned service containers, and a developer runs them locally with the server reachable. The
  ack **policy** — the one bit of decision logic — is a pure function tested in the ordinary `test`
  job, so a broker is needed only to prove the wire, not the rule.
- **Adapter logic under test → in process.** `metrics-vm`'s contract is its own queueing and
  back-pressure, not VictoriaMetrics' storage, so its suite runs against an in-process capturing
  transport in the ordinary `test` job, and a separate in-process HTTP mock pins the exact import
  bytes. No external VictoriaMetrics is needed to verify the adapter, which matches how little of it
  is VictoriaMetrics-specific.

**Consequences.**

- `async-nats` and its transitive tree (a rustls stack, `nkeys`, its own `tokio` features) join the
  dependency surface — the one large addition here, and `cargo-deny` reviews it like any other. The
  edge links it too, because the store is what publishes. Three consequences fall out of that tree,
  each recorded where CI enforces it:
  - It is pinned to a version (**0.50**) whose `rustls-webpki` is on the patched `0.103` line;
    the older `0.38` pulled a `0.102` webpki carrying fresh 2026 RUSTSEC advisories, so this is a
    security floor, not a preference.
  - Its `tokio-websockets` transport pulls **`webpki-roots`** (Mozilla's root-CA bundle), licensed
    `CDLA-Permissive-2.0` — a permissive *data* licence with no copyleft. It is admitted as one
    scoped, reviewed `deny.toml` exception rather than added to the workspace `allow` list. **A
    reviewer should confirm this licence is acceptable for the shipped cloud binary.** It disappears
    if the cloud is later configured to trust only the OS certificate store.
  - The stack straddles several crate versions the rest of the tree already uses (rand, rustls
    pieces, thiserror); a single `skip-tree = { crate = "async-nats" }` in `deny.toml` collapses
    those into one reviewed entry.
- `hmac` and `sha2` (0.10 line, matching what is already in the tree) join for S3 signing; no HTTP
  client crate is added for `blob-garage` or `metrics-vm`.
- Hand-written SigV4 and HTTP mean a wire bug reaches a test rather than the compiler; the contract
  suite against a real S3 server, and the in-process HTTP mock for the metrics wire, are the nets.
- None of this is foreclosed: each adapter hides its transport behind a port, so swapping the
  hand-rolled S3 for an SDK later, or `metrics-vm` for an OpenTelemetry exporter, changes one crate
  and nothing that depends on it. `blob-garage` is expected to be *deleted*, not swapped.
