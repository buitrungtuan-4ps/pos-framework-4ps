# ADR-0018 — Edge HTTP/WebSocket stack: axum, a broadcast fan-out, an embedded UI

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Relates to** [ADR-0002](0002-one-binary-per-tier.md) · [ADR-0013](0013-async-strategy.md) · [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0001](0001-offline-first-store-autonomy.md)

**Context.** `pos_edge` (P5) is the machine that actually sells. It serves the operator UI to
every device on the store's LAN — POS terminals, tablets, phones, the kitchen display — and it
must push a change one device makes (a line added, a course fired, a table paid) to every other
device fast enough that two people ringing up the same table never see stale state. The archive's
figure is a WebSocket fan-out **under 50 ms**; the capacity envelope
([`capacity-and-reliability.md`](../capacity-and-reliability.md)) treats that as the interactive
budget for the LAN.

Three constraints bound the choice.

- **It is one static binary** ([ADR-0002](0002-one-binary-per-tier.md), and the README's promise of
  "two static Rust binaries"). The compiled SolidJS UI ships *inside* the executable, not as a
  directory the operator has to copy next to it — a store server is a machine nobody administers, and
  "the UI files went missing" is not a support call we will accept.
- **It runs with the cable unplugged** ([ADR-0001](0001-offline-first-store-autonomy.md)). Nothing in
  this stack may reach for the network to serve a page or open a socket; the HTTP server and the
  fan-out are entirely local.
- **The runtime stays out of the domain** ([ADR-0013](0013-async-strategy.md)). `pos-core` is sans-I/O
  and synchronous; the async runtime, the HTTP framework and the socket layer are the binary's
  concern, behind the ports. The dependency-rule test
  ([ADR-0021](0021-corrected-port-list.md)) is what keeps them there.

This ADR governs the **edge's internal, UI-facing** transport only. The cloud's public `/v1` API and
its generated OpenAPI are a separate decision ([ADR-0019](../adr/README.md), P7); nothing here is a
public contract, so it carries no OpenAPI surface.

**Options considered.**

1. **`actix-web`.** Mature and fast, but it brings its own actor runtime and a heavier programming
   model than a request-response LAN server needs. Rejected: a second runtime alongside the tokio the
   adapters already assume ([ADR-0013](0013-async-strategy.md) names sqlx, NATS and reqwest as
   async-native) is exactly the kind of avoidable weight [ADR-0007](0007-in-house-vs-dependency.md)
   argues against.
2. **`warp`.** Filter-combinator design produces deeply nested types that are hard to name and hard to
   read in error messages. Rejected on maintainability.
3. **Raw `hyper` + a hand-rolled router.** Maximum control, but the router, the extractors, the
   WebSocket upgrade and the middleware plumbing are precisely the undifferentiated work a thin, vetted
   layer should absorb. Rejected: this is not the boundary we own.
4. **`axum` on `hyper` + `tower`, WebSockets via `axum::extract::ws`, fan-out over
   `tokio::sync::broadcast`, UI embedded with `rust-embed`.** Chosen.

**Decision.**

*HTTP is **axum**,* on `hyper` and the `tower`/`tower-http` middleware stack. ADR-0013 already assumes
axum as the async HTTP layer; this record makes it the decision. Extractors and handlers are plain
`async fn` — no framework macro rewrites the handler — and the `tower` layers carry the cross-cutting
concerns: a request-id, a `tracing` span that **records no PII** (§ the tracing rule,
[`pos-spec.md`](../pos-spec.md)), a request timeout, and a concurrency limit so a burst of tablets
cannot exhaust memory (the bounded-everywhere rule).

*WebSockets are **`axum::extract::ws`***, the framework's built-in upgrade (tokio-tungstenite
underneath) — no additional socket dependency. A device opens one socket and receives a typed event
stream; it does not poll.

*Fan-out is a single **`tokio::sync::broadcast`** channel.* The application layer publishes each applied
state change once; every connected socket task holds a `broadcast::Receiver` and forwards to its
device. In-process channel delivery makes the 50 ms budget a non-event — the cost is a clone and a
send, not a round trip. The channel is **bounded**: a device that stalls (a tablet asleep in a drawer)
falls behind, receives `RecvError::Lagged`, and is told to resynchronise from a fresh snapshot rather
than the server buffering an unbounded backlog on its behalf. A slow client degrades itself, never the
store. This is the same bounded-channel discipline the writer thread uses
([ADR-0015](0015-sqlite-access.md)); memory is bounded by design, not by hope.

*The UI is embedded with **`rust-embed`**.* The `ui/` build output (`ui/dist`) is compiled into the
binary, so `pos_edge` is genuinely one file. A `dev-ui` cargo feature flips `rust-embed` to read from
disk, so a UI change is a browser refresh, not a Rust rebuild. `tower-http`'s `ServeDir` is **not** the
default path precisely because it would reintroduce the loose directory ADR-0002 rules out; it exists
only behind that dev feature.

*The runtime is **tokio**, multi-threaded, and it lives in the binary.* tokio becomes a
`workspace.dependencies` entry, but only binaries and adapters enable it; `cargo tree -p pos-core`
stays free of it, and the dependency-rule test fails the build if that ever changes. The binary is the
"thin application layer — load, decide, apply" [ADR-0013](0013-async-strategy.md) says each binary
needs: a request arrives, the binary reads current state through a port, calls the synchronous
`pos-core` `decide`, applies the returned `Decision` inside one transaction, and publishes the applied
events to the broadcast channel. The socket layer is downstream of the commit — a device is told a
thing happened only after it is durable.

**Consequences.**

- `pos_edge` is one static executable with its UI inside it; deploying the store is copying one file
  and installing a service, consistent with [ADR-0002](0002-one-binary-per-tier.md) and the
  cattle-not-pets posture ([ADR-0003](0003-cattle-not-pets.md)).
- The 50 ms fan-out budget is met by construction — an in-process broadcast — and the failure mode of a
  slow device is bounded memory plus a forced resync, not server bloat.
- axum, hyper, tower, tower-http, tokio and rust-embed are all added at the binary layer only. They are
  new third-party dependencies, so `cargo-deny` vets their licences and advisories on the same PR, and
  the dependency-rule test proves they never reach the backbone.
- The binary declares its **own** `[lints]` table (per the workspace manifest's layering note): the
  library-strict baseline that forbids `unwrap` in `pos-core` is relaxed where a binary's `main` and its
  startup path legitimately need it, while the money and no-PII lints stay on.
- Publishing to the broadcast channel *after* the commit is what keeps the socket stream honest under a
  crash: a device never sees an event the database did not keep. Recovery is a fresh snapshot over the
  same socket, which is the `Lagged` path already built for slow clients — one mechanism, two triggers.
- Cost: WebSocket clients must handle a resync message, and the UI (P6) must be able to rebuild a screen
  from a snapshot rather than assuming an unbroken event stream. That is a requirement we want anyway —
  it is the same capability a device needs after reconnecting to a store that kept selling while it was
  asleep.
