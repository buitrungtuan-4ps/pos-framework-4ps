# ADR-0087 — Wiring the store's two outbound rails: the order relay, and edge event publish

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-01
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) (the counter never waits on the cloud) · [ADR-0013](0013-async-strategy.md) (static dispatch, I/O at the binary layer) · [ADR-0015](0015-sqlite-access.md) (the store's single-writer log and its outbox) · [ADR-0026](0026-port-shapes.md) §5 (the edge is the real `OrderIn`) · [ADR-0053](0053-cloud-sync-port.md) / [ADR-0054](0054-edge-cloud-http-client.md) (the `CloudSync` port and its HTTP adapter) · [ADR-0061](0061-order-relay.md) (the durable per-store order queue this pulls from) · [ADR-0064](0064-edge-order-in.md) (the intake the relay feeds) · [ADR-0085](0085-edge-cloud-sync-transport.md) (E1; the `CloudHttpClient` this reuses) · [ADR-0086](0086-edge-keyvault-and-activation.md) (E2; the boot gate this sits behind) · `docs/roadmap-v3.md` (roadmap v3, slice E3)

**Context.** E1 and E2 gave the shipped edge a cloud: it dials the config tree, heartbeats, and can be activated from its own screen. Two rails the architecture has always assumed are still not connected in the binary.

*The order relay.* [ADR-0061](0061-order-relay.md) built a durable per-store queue in the cloud: a marketplace or `POST /v1/orders` caller parks an order, and the store **pulls** it, makes it, and acks — outbound-only, so no inbound port on the store LAN. Both halves exist. The cloud serves `GET /sync/stores/{store_id}/orders` (long-polling up to 20 s) and `POST /sync/stores/{store_id}/orders/{queued_id}/ack`, both requiring the `relay_orders` scope. The edge has a complete, tested `RelayClient` over a `RelayTransport` seam, and a complete, contract-tested `EdgeOrderIn`. **Neither is constructed anywhere in production code.** There is no `RelayTransport` implementation at all, and `EdgeOrderIn` — the tree's one real `OrderIn` — has never been built outside a test. A cloud-placed order reaches the queue and stops there.

*Event publish.* `docs/architecture.md` §6.2 and the [`MessageLink`](../../crates/pos-ports/src/message_link.rs) port describe the store's event flow: commit locally, publish the outbox to a durable stream, acknowledge what the stream accepted. The cloud already runs the consumer half — `NatsConsumer` is wired into `pos_cloud`'s `main.rs` and drives the ingest cursor. The edge end does not exist: `pos-edge` has no `link-nats` dependency, `NatsLink` is constructed only in tests, and `EventStore::outbox_batch` / `acknowledge_outbox` — the durability seam the whole design rests on — have **zero production callers**. The cloud is listening to a stream nothing publishes to; a store's events reach it only through the unauthenticated `/internal/ingest` reconciliation re-push, which no edge calls either. In practice: a sale committed at the counter never leaves the box.

E3 is the slice that connects both. It is one slice because they share the same three constraints — they run only behind E2's boot gate, they are outbound-only, and neither may ever block the counter.

**Decision.**

- **The relay transport is a third transport on the existing `CloudHttpClient`.** `RelayHttpTransport` joins `ConfigHttpTransport` and `HeartbeatHttpTransport` in `cloud_http.rs`, built from the same client and `StoreId`, implementing `RelayTransport::pull`/`ack` against the two `/sync` routes. No new dependency and no second HTTP stack: the raw hyper + tokio-rustls pin of [ADR-0038](0038-webhook-tls-sender.md)/[ADR-0054](0054-edge-cloud-http-client.md) already carries config-pull and heartbeat.

- **The relay pull gets its own, longer request timeout.** `CloudHttpClient`'s 15 s timeout is shorter than the cloud's 20 s long-poll cap, so a quiet store would time out every single poll and never see a parked order. The client gains a per-transport timeout so the relay can allow the full park plus margin (30 s) while config-pull and heartbeat keep the 15 s they want. This is a bug the wiring would otherwise inherit silently, which is why it is a decision and not an implementation detail.

- **One scoped store key, now carrying two scopes.** The relay authenticates with the same key E1/E2 resolve (vault `SecretName::SyncKey`, then `POS_EDGE_SYNC_KEY`); the key must hold **`relay_orders` alongside `read_config`**, which is exactly how `Scope::RelayOrders` is documented. A key missing the scope gets `403` and the relay loop retries on its 5 s backoff, logging each failure — the same behaviour config-pull already has for a bad key. Escalating backoff on an *authentication* failure (as against a transport one) is worth doing for both loops and is flagged below, not smuggled in here.

- **Relayed orders are opened by the box, under a derived system device id.** `EdgeOrderIn` records a `DeviceId` on the events an inbound order writes, because there is no signed-in employee. A relayed order came from the cloud, not from a paired device, so the honest answer to "which device did this" is *the store server itself*. The edge derives a stable, store-unique **system device id** from its `StoreId`, in the spirit of `StoreIdentity::for_store`'s documented bootstrap ids. Rejected alternatives are below; the point is that the box needs an identity for these events **today**, and the cloud's own device registry — not an edge-side guess — remains the authority on fleet identity.

- **`serve()` takes the queue-number authority; the relay loop is composed beside the E1 loops.** `EdgeOrderIn` needs an `Arc<Edge<S>>`, a `QueueNumberAuthority`, and that device id, and `S` must be an `IntakeLedger` as well as an `EventStore` (so the dedupe row lands in the order's own transaction, ADR-0064). `serve` therefore grows one parameter and one bound: the real binary passes a `SqliteStore` clone (its single writer thread is the durable daily counter), the on-fakes example passes `InMemoryQueueNumbers`. The loop spawns inside `spawn_cloud_loops`, so it inherits E2's gate untouched: no `cloud_url` → no relay; not activated → no relay; no sync key → no relay.

- **The edge publishes its outbox over the `MessageLink` port, with `link-nats` as the adapter.** A new event-publish loop in `pos-edge` runs the sequence the port's contract prescribes: handshake once, then `outbox_batch` → `publish` → `acknowledge_outbox` through the accepted position, repeating while the batch is full and idling when it is empty. `PublishOutcome.accepted` is a **prefix**, so the loop acknowledges exactly what was accepted and re-sends the tail; at-least-once, with the cloud's ingest already idempotent by `event_id`. `pos-edge` gains a `link-nats` path dependency — the `async-nats` subtree is already in the lock via `pos-cloud`, so this adds no new third-party crate to the workspace, and the dependency rule is untouched (`pos-core`/`pos-proto` see nothing).

- **`Edge<S>` exposes its store read-only.** The publish loop needs an `EventStore` handle and today `Edge` owns `S` privately behind `fanout()`/`store_id()`. A `store()` accessor is the smallest honest seam — the log the `Edge` owns, lent to the shipper — and avoids threading a second store handle through `serve`.

- **NATS is configured like the cloud, credentialed like the sync key.** `EdgeConfig` gains an optional `[nats]` section carrying the **stream and subject only** — they must line up with the cloud consumer's `stream`/`filter_subject`, and they are not secrets. The server URL comes from `POS_EDGE_NATS_URL` in the environment (the same optional root-owned mode-0600 `/etc/pos-edge/env` the sync key uses), because a NATS URL is where a credential would be embedded and [ADR-0086](0086-edge-keyvault-and-activation.md)'s rule is that a credential never lives in `config.toml`. Absent URL or absent `[nats]` → no publish loop, logged, and the box trades exactly as before.

- **Neither rail may block the counter.** Both loops are `tokio::spawn`ed background tasks sharing the server's shutdown watch, and every failure path is a log plus a backoff. A cloud that is down, a stream that is full, a key that is wrong: the outbox grows, the queue stays parked, and the store keeps selling ([ADR-0001](0001-offline-first-store-autonomy.md)). The outbox is *why* this is safe — nothing is acknowledged until the stream has it.

**Deliberately deferred (flagged, not silently dropped).**

- **Escalating backoff on an authentication failure.** A key missing `relay_orders` (or a revoked key) currently produces a warn every 5 s from the relay loop, and config-pull has had the same shape since E1. The fix — distinguish "the cloud refused me" from "the cloud is unreachable" and back off to minutes for the former — belongs to both loops at once, as its own slice.
- **NATS credentials in the keyring.** The server URL rides the mode-0600 env file, matching the sync key's interim. Moving both into the OS keyring (a `SecretName` variant) is the follow-up to [ADR-0086](0086-edge-keyvault-and-activation.md)'s credential story, and it lands with the headless-Linux durability work rather than ahead of it.
- **A real fleet identity for the box.** The derived system device id is honest about being the store server, but the durable answer is the device id the cloud granted at activation, carried forward on the box. That needs somewhere to keep it (it is an identifier, not a secret, so the vault is the wrong home) and is worth doing with the device-credential-on-`/sync` work [ADR-0085](0085-edge-cloud-sync-transport.md) already flagged.
- **End-to-end proof against a live cloud and a live NATS.** Both loops are exercised against fakes in the pull-request gate; a store box pulling a real parked order and a real stream draining a real outbox is the integration/hardware lane, alongside the keyring gate.
- **`/internal/ingest` stays the reconciliation re-push.** It remains unauthenticated and out of OpenAPI; hardening or removing it is [ADR-0078](0078-sync-and-ota-closure.md)'s reconciliation work, not this slice's.

**Rejected.**

- **A second HTTP client for the relay** — rejected: `CloudHttpClient` already speaks the `/sync` surface with the right bearer and TLS pin. A per-transport timeout is a two-line change; a second stack is a second thing to keep secure.
- **Shortening the cloud's long-poll to fit the edge's 15 s timeout** — rejected: the park is what makes the relay near-real-time without hammering. The client is the side that should accommodate it, and only for the one route that parks.
- **Publishing events over HTTP to `/internal/ingest` instead of NATS** — rejected: it is the reconciliation re-push, unauthenticated and deliberately undocumented, and the cloud's primary path (`NatsConsumer` → the ingest cursor) is already running in production. Making the edge's normal path the emergency path would leave the real one permanently unproven.
- **Draining the outbox inside the commit path** — rejected: it would put a network round-trip on the sale. Commit → publish → acknowledge, out of band, is exactly what the outbox exists for ([`message_link.rs`](../../crates/pos-ports/src/message_link.rs) is explicit that there is no transaction spanning NATS and SQLite).
- **Acknowledging the whole batch when `publish` reports a partial accept** — rejected: it would silently lose the tail. `PublishOutcome.accepted` is a prefix count and the loop treats it as one.
- **Making the relay's device id a zero/sentinel ULID shared by every store** — rejected: it would collide across stores in the cloud's own analytics. Deriving from the `StoreId` keeps it stable *and* unique, and says what it means.
- **Reading the granted device id back out of the store's own event log** — rejected for this slice: `device.activation.completed` can sit anywhere in a log that has been trading offline since before activation, and `EventQuery` pages forward only, so "find the activation event" is an unbounded scan at every boot. The durable fix is to carry the id forward at activation (flagged above), not to hunt for it.
- **Gating local trading on either rail** — rejected, as ever ([ADR-0001](0001-offline-first-store-autonomy.md)).

**Consequences.**

- **A cloud-placed order finally reaches the kitchen.** The marketplace/`POST /v1/orders`/QR path — cloud queue → store pull → reprice against the store's own menu → open in the local log → queue number → ack — runs end to end in the shipped binary for the first time. `EdgeOrderIn`, contract-tested since ADR-0064, gets its first production caller.
- **A committed sale finally leaves the box.** The outbox stops being a write-only table: `outbox_batch`/`acknowledge_outbox` get their first production callers, and the cloud's already-running consumer has a publisher.
- **No wire, protocol, permission, or migration change.** The `/sync` relay routes, the `MessageLink` port, and the outbox contract are all as built. E3 is composition plus one new transport, one new loop, one `serve` parameter, one `Edge` accessor, and an optional config section. No `pos-proto` change, no `PROTOCOL_VERSION` bump.
- **No new third-party dependency.** `link-nats` is a workspace path crate and its `async-nats` subtree is already in the lock through `pos-cloud`; `cargo deny` sees no new advisory or licence surface. The dependency rule holds — everything lands at the binary layer.
- **One more thing a provisioning key must carry.** A store key issued with `read_config` alone now leaves the relay dark. The provisioning guide and the store-key issuance path must say `read_config` **and** `relay_orders`, and a misprovisioned key is visible as a repeated `403` in the edge log rather than as silence.
- **Delivery shape.** This ADR is PR A. The implementation follows as: the relay wiring (`RelayHttpTransport`, the per-transport timeout, the system device id, the `serve` parameter, the loop) — then the event-publish loop (`link-nats`, `Edge::store()`, the `[nats]` config, the drain loop), each behind the same green gate, each with its own deploy-doc update.

---

**Amendment 1 (2026-09-04) — one fleet stream on one subject, because the cloud runs exactly one
durable consumer.**

Generating the `[nats]` section from the console (roadmap **E3**'s remaining half) needs a *value*,
and the tree carried two conventions that cannot both be true.

- The edge's `NatsConfig`, `link-nats`'s `NatsConfig` and `ConsumerConfig` all say **"one per store,
  e.g. `POS_STORE_<id>`"**.
- `bootstrap.sh` writes the cloud's arming instruction as **`stream = "POS_FLEET"`** with
  `filter_subject` left empty, and `pos_cloud` binds **one** durable consumer to **one** named
  stream. A fleet of per-store streams would therefore be ingested one store deep: whichever store
  `cloud.toml` happens to name.

This decision's own wording — "they must line up with the cloud consumer's `stream`/`filter_subject`"
— only has one solution while the cloud reads a single stream. **Every store publishes into
`POS_FLEET` on the subject `pos.fleet.events`.**

- **The subject is shared, not per-store, and that is the load-bearing part.** `NatsLink`'s handshake
  calls `get_or_create_stream` with `subjects: vec![subject]`, and JetStream's *create-or-get* does
  **not** reconcile an existing stream's subject list. Had each store kept its own subject inside a
  shared stream, the first box to connect would have fixed the capture to its own subject and every
  other store's publish would have been refused by a broker that has no stream for it — a failure
  that appears only on store number two, in production, weeks after the console was tested against
  one shop. With identical stream *and* subject on every box the call is genuinely idempotent
  fleet-wide: the first store creates the stream, the rest find it, and every event lands.
- **Nothing needed the store id in the subject.** The cloud's `filter_subject` is empty, ingest is
  idempotent by `event_id`, and every `EventEnvelope` already carries its `store_id` — which is what
  the rollups and the reconciliation read. A per-store subject would have been an identity the bus
  did not actually enforce: the broker token is one fleet-wide credential
  ([ADR-0089](0089-edge-event-bus-transport.md) flags exactly this), so any box could publish on any
  other box's subject. Real per-store identity on the bus arrives with per-store credentials, and
  the subject layout is worth revisiting *then*, together with them, rather than guessed at now.
- **The console cannot fill in `POS_EDGE_NATS_URL`, and deliberately does not try.** The broker token
  lives in `deploy/secrets/nats.conf` on the cloud box. It is **fleet-wide**, unlike the per-store
  scoped sync key the wizard does emit, so putting it in a browser and then into every store's env
  file would spread one credential across every machine in the estate. The generated `env` therefore
  carries the line commented, in the shape that works (`tls://:<token>@<host>:4222`) and with the one
  command that recovers the token, and the operator completes it. A `[nats]` section with no URL logs
  a **warning** naming the missing variable, which is the honest state: configured, not yet armed.

**What this narrows.** `NATS_MAX_MESSAGES` (1 000 000) and `NATS_MAX_BYTES` (1 GiB) were sized as a
*per-store* ceiling and are now a *fleet* one, and `discard: new` means a full stream refuses new
messages rather than dropping old ones — so the fill rate is a fleet property, and the outbox is what
holds while an operator raises the limits. That is visible (the 80% capacity alert,
[ADR-0073](0073-alerting.md)) and lossless, not silent, but the figures now want sizing against a
real estate rather than one shop. That is the already-planned **A·P4 O4** JetStream capacity probe,
and this amendment is the reason it is no longer optional for a fleet above a few dozen stores.
