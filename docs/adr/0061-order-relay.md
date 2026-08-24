# ADR-0061 — The cloud→store order relay is a durable per-store queue the store pulls, and the cloud implements `OrderIn` over it

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-24
**Relates to** [ADR-0056](0056-public-order-intake.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0001](0001-offline-first-store-autonomy.md) · [ADR-0033](0033-config-tree.md) · [ADR-0037](0037-api-keys.md) · [ADR-0039](0039-config-delivery.md)
**Extended by** ADR-0062 (the optional low-latency `live` mode — forthcoming, in the follow-up PR)

**Context.** [ADR-0056](0056-public-order-intake.md) built the public intake *library* — `POST /v1/orders`
mapping a request to an [`InboundOrder`], binding the store to the caller's tenant, and calling the
`OrderIn` port — and stated that "in the binary the `OrderIn` it calls is the cloud→store relay",
leaving that relay's design to this ADR (`docs/roadmap.md` P11a-2). The relay is the one buildable
piece the served route waits on.

Two fixed facts shape it:

1. **The store's edge is the real `OrderIn` implementor** ([ADR-0026](0026-port-shapes.md) §5): it
   reprices from its own menu, routes to the kitchen, and **must accept offline**
   ([ADR-0001](0001-offline-first-store-autonomy.md), contract rule 5). The cloud cannot decide an
   acceptance; it can only get the order to the store and relay what the store decided.
2. **The store is outbound-only, by design.** `docs/architecture.md` §3 — "stores dial out only,
   which solves 4G and CGNAT without port forwarding or VPNs" — and the `MessageLink` port is
   one-directional: the store never waits on the cloud and the cloud never pushes down the link.
   There is no cloud→store channel, and re-introducing one (a reachable IP + port-forward) would undo
   the property that lets a store run on a bare 4G SIM.

**Decision.**

- **A durable per-store order queue, which the store pulls.** The cloud persists each inbound order in
  an `order_queue` table (an `OrderQueueStore` seam, backed by `store-postgres`, RLS-isolated by
  tenant like every other cloud table). The store fetches pending orders over its **own outbound**
  sync channel and reports the outcome back up. Nothing is pushed into the store, so "stores dial out
  only" holds unchanged — the relay is delivery over the existing store-pull posture, not a new link.

- **The cloud implements `OrderIn` over that queue** (`crate::relay::OrderRelay`), so the ADR-0056
  served route composes it with no change:
  - `submit(order)` — **idempotent enqueue** on `(tenant, store_id, sales_channel, external_reference)`
    (the port's own key, [ADR-0026](0026-port-shapes.md)); then **park** up to the store's configured
    deadline, waiting for the store to report an acceptance. If it arrives in time → `Ok(acceptance)`
    (a `201`/`200`, the synchronous experience). If the deadline passes → `Err(PortError::unavailable)`
    (a `503`) — **and the order stays queued**, so a store that was briefly offline still makes it.
  - `look_up(store, channel, reference)` — reads the row's recorded acceptance: `Ok(Some(..))` once the
    store has reported, `Ok(None)` while still pending or unknown. This is precisely the port's stated
    "resolution path for a caller whose submit timed out: it can ask rather than retry", served as
    `GET /v1/orders`. A timed-out caller polls it instead of re-submitting (which idempotency would
    dedupe anyway).

- **The store-facing surface is store-initiated and API-key-scoped** ([ADR-0037](0037-api-keys.md)),
  under a new deny-by-default `Scope::RelayOrders` (`relay_orders`) the store's key holds:
  - `GET /sync/stores/{store_id}/orders` — a **bounded long-poll**: returns the pending batch
    immediately if any, else holds the request open up to a short cap before returning empty, so a
    reachable store picks an order up in well under a second without hammering the endpoint.
  - `POST /sync/stores/{store_id}/orders/{queued_id}/ack` — the store, having run the order through its
    **local** `OrderIn`, reports the resulting `OrderAcceptance` (or a refusal). The cloud records it,
    which unblocks the parked `submit` and answers `look_up`.

- **`StoreDirectory` (ADR-0056's tenant-binding seam) is backed by `config_trees`.** A store is known —
  and its owning tenant resolved — once configuration has been published to it (`SELECT tenant_id FROM
  config_trees WHERE store_id = $1`). Configuring a store before it takes orders is the natural order of
  operations, and it needs no second index.

- **A per-store toggle, published from the cloud through the config tree** ([ADR-0033](0033-config-tree.md)):
  `store.order_relay.enabled` (default `true`; `false` → `submit` is `FailedPrecondition`, a `409`) and
  `store.order_relay.wait_ms` (how long `submit` parks; default 3000). Because it is config, the operator
  changes it per store from the dashboard, and the store reads it over the same `/sync` pull it already
  uses — no deploy, no per-store code.

**Rejected.**

- **The relay implementing `OrderIn` but returning a bespoke "queued" acceptance** — rejected. `OrderIn`
  returns a definitive `OrderAcceptance`; a store that has not answered has not accepted anything, and
  inventing a fake acceptance would put an unconfirmed order in front of a caller as confirmed. The
  timeout is an honest `Unavailable`, and the durable queue plus `look_up` is the real resolution — the
  path the port was designed with.
- **A cloud→store push (reachable IP / port-forward / VPN)** — rejected: it breaks "stores dial out
  only", the property that makes a 4G store deployable with no site networking, to serve a minority of
  stores. The synchronous experience is achieved by the store's *outbound* pull being fast (long-poll),
  not by the cloud reaching in.
- **A cloud-minted relay id as the caller's handle** — rejected as redundant: the caller already owns
  `(sales_channel, external_reference)`, which is the idempotency key and the `look_up` key. The
  internal `queued_id` exists only for the store's ack path.
- **A synchronous request/reply over a store-held live connection (Mức 2)** — *deferred*, not rejected:
  it lowers latency below what long-poll gives but requires the store to hold a live channel and reply,
  which amends `MessageLink`'s one-directional rule. It is a conscious, separate decision (ADR-0062,
  forthcoming) layered behind a `store.order_relay.mode` config flag, over this same queue as the
  durable fallback.

**Consequences.**

- QR ([ADR-0012](0012-qr-ordering-via-cloud.md)) and marketplace orders reach the kitchen whether the
  store is instantly reachable (fast `201` via long-poll) or briefly offline (queued, delivered on
  reconnect), with no order lost and no cloud→store connection.
- Idempotency makes every retry and the queued fallback safe: a resubmit, or a store that processed a
  pulled order and then failed to ack, converges on one order by the `(channel, reference)` key.
- The per-store behaviour is a cloud-published config value, so "turn intake on/off, or tune the wait,
  for this one store" is a dashboard action, exactly the control an operator expects.
- `POST /v1/orders` and `GET /v1/orders` now answer in the binary and enter `docs/openapi.json`; the
  store-facing `/sync/.../orders` routes are internal to the fleet and stay out of the public document,
  as the other `/sync` routes do.
- The cloud now holds unconfirmed order contents transiently (until acked and then per retention). A
  guest `note` rides as a [`GuestNote`], which still cannot enter the event log; the queue is order
  delivery, not the log, and is masked/aged under the same retention posture ([ADR-0035](0035-retention-and-pii-masking.md)).
