# ADR-0062 — The relay's latency is not on the wire, so there is no live link; the poll loop is the defect

**Status** Accepted · **Owner** @maintainers-cloud · **Date** 2026-09-05
**Relates to** [ADR-0061](0061-order-relay.md) (the relay this completes) · [ADR-0001](0001-offline-first-store-autonomy.md) (offline-first) · [ADR-0026](0026-port-shapes.md) (`OrderIn`, `MessageLink`) · [ADR-0087](0087-edge-relay-and-event-publish.md) Amendment 1 (one fleet stream, one subject) · [ADR-0089](0089-edge-event-bus-transport.md) (the bus transport, and its unwritten-ADR forward reference) · [ADR-0016](0016-postgres-access.md) (tokio-postgres behind deadpool)

This is the record [ADR-0061](0061-order-relay.md) reserved the number for and nobody wrote. Its
header said the follow-up "was to be ADR-0062", that the number is reserved, and that until it exists
`MessageLink` stays one-directional and the relay stays long-poll. That sentence has been true for
long enough to be worth resolving rather than deferring again.

**It does not decide what everyone expected.** The task list, `docs/roadmap.md` §P11a-2 and
ADR-0061's own *Rejected* section all describe 0062 as the record that would *amend* `MessageLink`
to carry a live cloud→store channel. Having measured the thing, that is the wrong change, and this
ADR refuses it on merit rather than deferring it a third time.

## The problem

ADR-0061 built the relay as a durable per-store queue the store pulls, and parked the caller's
`submit` until the store reported back. It deferred a "Mức 2" live mode — "a synchronous
request/reply over a store-held live connection" — as a conscious, separate decision, on the stated
ground that it "lowers latency below what long-poll gives".

That framing put the cost on the wire. The code says otherwise, and it says something worse.

**The relay has two polling loops, and both are a fixed 100 ms re-read of a Postgres table.**

```rust
// crates/pos-cloud/src/relay.rs
const DEFAULT_WAIT_MS: u64 = 3000;                            // the park deadline
const POLL_INTERVAL: Duration = Duration::from_millis(100);   // both loops
const LONGPOLL_CAP: Duration = Duration::from_secs(20);       // the store-facing hold
```

* The **submit park** re-reads `outcome(...)` every 100 ms until the store reports — about **30
  reads per parked order**, the first no sooner than 100 ms after the row was written.
* The **store-facing long-poll** (`GET /sync/stores/{store_id}/orders`) re-reads `pull_pending(...)`
  every 100 ms for up to 20 seconds, then answers `[]` and the store immediately asks again.

The second one is the finding. There is no `LISTEN`/`NOTIFY`, no jitter, and no per-store gate, so
**an idle store issues about ten queue queries per second against Postgres, for as long as it is
switched on.** A store that sells nothing all night costs the same as one at peak. At the 500-store
fleet `docs/capacity-and-reliability.md` sizes for, that is on the order of **5,000 queries per
second of pure idle load**, through a pool of `POOL_SIZE = 16` connections shared with ingest, the
rollup projector, webhooks, alerting, retention and the whole console.

The latency a live link was supposed to buy is, by comparison, small and mostly not on the wire: the
100 ms poll granularity on each leg. Everything else in the path — the store's own HTTP round trip,
its reprice, its kitchen routing, and the human who confirms the order — is untouched by how the
cloud learns there is a row to read.

So the deferred question was aimed at the wrong target. The relay does not need a faster channel. It
needs to stop asking a database, ten times a second, whether anything has happened.

## The decision

**1. There is no live cloud→store connection. Refused on merit, not deferred.**

Three independent reasons, any one of which is sufficient:

* **The cloud cannot dial a store, and must not learn how.** `docs/architecture.md` §3 — "stores dial
  out only, which solves 4G and CGNAT without port forwarding or VPNs" — and ADR-0061 restates it:
  re-introducing a reachable IP and a port-forward "would undo the property that lets a store run on
  a bare 4G SIM". Nothing in `deploy/edge/` documents an inbound path, because there is deliberately
  none.
* **A store-held outbound channel is buildable, and still wrong here.** It is a *second delivery
  path* for orders that the durable queue already delivers. Two paths that can disagree about whether
  an order was handed over is a worse property than 100 ms of latency, and it brings keepalives,
  half-open sockets on a reaped 4G NAT binding, backpressure, and a reconnect storm at every deploy —
  all to remove a delay that is not where the time goes.
* **The event bus must not carry it, on credentials.** The tempting version is a per-store *core*
  NATS subject with the cloud issuing `request` and the store answering on the reply subject. Be
  precise about why that fails, because the obvious objection is not the real one:
  [ADR-0087](0087-edge-relay-and-event-publish.md) Amendment 1 bars a per-store subject *inside the
  fleet JetStream stream* — `ensure_stream` is a create-or-get and the first box to connect fixes the
  subject list — but a core subject creates no stream and is not barred by it. What bars it is the
  credential. `deploy/bootstrap.sh` writes the broker an `authorization { token: … }` with **no
  permissions block**, ADR-0089 records per-store mTLS as a later slice and ADR-0097 defers it again,
  so every box holds the same fleet-wide token with no per-subject restriction. A queued order
  carries `GuestNote` — the one field the relay deliberately keeps out of the event log — plus the
  table and the whole basket. On that bus any store could subscribe to another store's order inbox,
  and, worse, **publish** onto it: a fabricated ticket in someone else's kitchen. That is a
  tenant-isolation regression, not a latency improvement, and it would have to be undone before the
  live mode could ship rather than after.
* **And it would couple order delivery to an optional link.** ADR-0087 Amendment 1 deliberately keeps
  the console from emitting `POS_EDGE_NATS_URL`, because the broker token is fleet-wide and a browser
  is the wrong place for it — so the generated `env` carries that line commented and an operator
  finishes it by hand. A store where nobody completed that step trades and takes QR and marketplace
  orders perfectly well today; the bus's absence costs analytics only. Move order delivery onto it
  and that same store silently stops receiving orders. For a framework whose audience is forks and
  rollouts, that is exactly the wrong failure to introduce.

**2. `MessageLink` stays one-directional, and the hole at 0062 closes with a decision.** The port
keeps its four methods and its "no `subscribe`, no `receive`, no callback" shape. ADR-0061's
*Rejected* entry and its header are amended by this record: the extension is not unwritten any more,
it is declined.

**3. The two poll loops become one wake.** The cloud already knows the instant a row appears — it is
the process that wrote it. `RelayWake` is a small seam over that fact:

```rust
/// Wakes a waiter the moment the row it is parked on exists. The relay writes the row and then
/// signals; nothing polls to discover a write this same process performed.
pub trait RelayWake: Send + Sync {
    /// Signals that `store` has at least one newly queued order.
    fn queued(&self, tenant: TenantId, store: StoreId);
    /// Signals that the store reported an outcome for `queued_id`.
    fn reported(&self, tenant: TenantId, queued_id: QueuedOrderId);
    /// Waits for the next `queued` for this store, or `timeout`, whichever comes first.
    async fn await_queued(&self, tenant: TenantId, store: StoreId, timeout: Duration) -> Woke;
    /// Waits for the next `reported` for this order, or `timeout`.
    async fn await_reported(&self, tenant: TenantId, queued_id: QueuedOrderId, timeout: Duration) -> Woke;
}
```

**The two waiter classes get separate signals.** A parked `submit` is waiting for an *outcome* on one
order; a long-polling store is waiting for an *enqueue* on any of its orders. One shared notification
would wake the wrong one and leave the right one asleep until its fallback fired.

**A waiter subscribes before it reads, never after.** The window between "I read and found nothing"
and "I began waiting" is exactly where a signal goes missing, and a missed signal on the submit leg
is a `503` for an order the store did accept. Both loops therefore take their subscription first,
then read, then wait — so a row written in between is already accounted for.

**4. The fallback poll stays, and is bounded so it cannot be switched off by arithmetic.** A wake is
an optimisation, never the correctness argument: the row in Postgres remains the only source of
truth, and every waiter still re-reads on a timer as well as on a signal. The park's re-read count is
`wait / interval`, so an interval longer than the deadline computes to **zero re-reads** and silently
removes the safety net. The interval is therefore clamped to at most half the park deadline, and a
test pins that a lost signal still resolves the park.

**5. The wake is in-process, and the seam is why that is not a corner.** `deploy/compose.yml` runs one
container per service and every deployment in `k8s/` declares `replicas: 1`, so every writer and every waiter are
in the same process today and a `tokio::sync::Notify` is not merely sufficient, it is exactly right.
A second instance would need the signal to cross processes — Postgres `LISTEN`/`NOTIFY` is the
obvious implementation — and that is a new `RelayWake` impl behind the same seam, not a change to the
relay. Building the cross-process version now would mean a trigger, a migration, a dedicated
connection held outside the `deadpool` pool (which spawns each connection's driver task and would
otherwise swallow the notification), a keepalive for that connection, and a NOTIFY payload naming a
tenant on a channel that RLS does not scope — real machinery, all of it, for a deployment shape that
does not exist.

**6. The store's reconnect backoff gets jitter.** `RETRY_BACKOFF` is a fixed five seconds with no
jitter, and the cloud deploys with `Recreate`, so every store in the fleet is disconnected at the
same instant and retries in lockstep for as long as the cloud takes to come back. That is the storm
an operator actually gets paged for, and it is three lines to fix.

## What this deliberately does not do

* **It does not introduce `store.order_relay.mode`.** ADR-0061 §83 and `docs/roadmap.md` §470 both
  promised the live mode would sit behind that config key. There is no live mode, so there is no mode
  to select; the key is not added, and the two promises are corrected rather than left dangling for
  the next reader to hunt. `store.order_relay.{enabled,wait_ms}` are unchanged.
* **It does not make the relay faster than the store.** The remaining latency is the store's own
  round trip and its intake; a wake removes the cloud's contribution and nothing else. Anyone
  expecting a step change in what a guest experiences should read §"The problem" again.
* **It does not touch the ack leg's transport.** Per-call connection setup in the edge's HTTP client
  is a real cost on the order path and a bigger one than the poll granularity, but it belongs to the
  transport, not the relay, and it is a separate change with its own measurements.
* **It does not add a metric.** The idle-query rate this removes is worth watching, but the metrics
  port's sparse-sampling profile is its own decision and this record does not pre-empt it.
* **It does not revisit per-store credentials.** A live channel would have forced that question;
  refusing the channel leaves ADR-0089's deferred per-store mTLS exactly where it was.

## Consequences

* An idle store stops costing about ten Postgres queries a second. The fallback interval is the new
  floor, and it is a configured value rather than a constant, so an operator can trade idle cost
  against how long a lost signal can go unnoticed.
* A parked `submit` now resolves as soon as the store reports, rather than at the next 100 ms tick,
  and the store's long-poll returns as soon as an order is queued.
* The relay keeps exactly the wire it had. No route, no header, no scope, no `PROTOCOL_VERSION`
  change, no migration, and no new dependency — `tokio`'s `sync` and `time` features are already in
  the tree.
* A second cloud instance is now a `RelayWake` implementation rather than a redesign, and the seam
  documents what that implementation has to do.
* **There is no listener that can die quietly.** A cross-process wake would add a connection whose
  silent death moves the whole fleet from prompt delivery to the 20-second cap — worse than today,
  and invisible, which is the failure mode this programme keeps paying for. An in-process notify
  cannot fail without the process failing. Whoever implements the cross-process version inherits that
  obligation: it needs a tick on the background-task health surface and an alert, and this record is
  where that requirement is written down.
* The hole at 0062 is closed. `docs/adr/README.md` gains its row, and ADR-0061's header stops
  pointing at a decision nobody had made.
