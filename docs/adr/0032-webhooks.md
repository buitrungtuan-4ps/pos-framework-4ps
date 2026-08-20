# ADR-0032 — Webhooks are a signed, SSRF-guarded cursor over the event log

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0019](0019-openapi-generation.md) · [ADR-0022](0022-events-partition-strategy.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0031](0031-cloud-adapter-transports.md)

**Context.** A tenant integrating with the cloud wants to be *told* when things happen rather than
poll the `/v1` API. So the cloud offers webhooks: register a URL, receive each new event for a store
as an HTTPS `POST`. Three things make this more than a for-loop over HTTP calls, and each is a way to
get it dangerously wrong:

1. **A slow or dead receiver must not cost the cloud memory.** The exit criterion (`docs/roadmap.md`
   P7) is explicit: *a dead webhook endpoint falls behind without any memory growth*. A naive
   in-memory delivery queue fails this the first time an endpoint goes down for a weekend.
2. **The receiver must be able to trust what it receives.** An unauthenticated `POST` to a URL is
   forgeable by anyone who learns the URL, and replayable by anyone who captures one delivery.
3. **The destination URL is attacker-controlled.** A tenant types it into a form. Unchecked, it is a
   server-side request forgery primitive aimed at the cloud's own metadata service, database, and
   private network.

**Decision.**

- **A webhook is a cursor over the event log, not a queue.** Each endpoint stores only its position
  in the durable log (`store-postgres`, [ADR-0022](0022-events-partition-strategy.md)); the events
  themselves are already there. Delivery reads one bounded page after the cursor, delivers it, and
  advances the cursor **only on success**. A failed delivery leaves the cursor untouched and buffers
  nothing — the backlog lives in PostgreSQL, which was going to hold it regardless — so a dead
  endpoint's memory cost is one page, forever. This is the same "cursor over the log" shape as the
  NATS ingest consumer ([ADR-0031](0031-cloud-adapter-transports.md)); webhooks are a *reader* of the
  same log the `/v1` API reads.

- **Every delivery is HMAC-signed over a timestamped payload, with a ±5-minute replay window.** Two
  headers ride each `POST`: a Unix-seconds timestamp and `v1=<hex>`, where the hex is
  `HMAC-SHA256(secret, "{timestamp}.{body}")` under a per-endpoint secret. Binding the timestamp into
  the signed bytes is what makes the window real — a captured delivery cannot be re-stamped without
  breaking the signature, and cannot be replayed under its original stamp once it is more than five
  minutes old. The `v1=` prefix leaves room for a future scheme. The reference receiver check
  (`webhook::sign::verify`) compares in constant time and enforces the window; it is the exact
  algorithm an integrator implements, so it is code, not prose.

- **Every destination is SSRF-vetted before registration and before each connection.** The policy
  (`webhook::ssrf`): the scheme must be `https` (a webhook carries business data; plaintext is
  refused), the authority must carry no `user:pass@` credentials, and **every** address the host
  resolves to must be public unicast. The address check is a blocklist of the ranges an SSRF payload
  aims at — loopback, RFC-1918 private, `169.254/16` link-local (the cloud metadata range), CGNAT,
  unique-local and link-local v6, IPv4-mapped v6, documentation, benchmarking, reserved, multicast —
  and a hostname that resolves to *any* forbidden address is refused whole, which defeats a
  public-looking name that points inward. Because DNS can change between the check and the connect,
  vetting resolves the host and the transport connects **only to those already-vetted addresses**,
  never re-resolving; that closes the rebinding gap.

- **Each endpoint has a circuit breaker with a 24-hour auto-disable.** Five consecutive failures open
  the breaker for a cooldown (so a dead endpoint is polled once per cooldown, not hammered); a
  half-open trial then probes recovery. An endpoint that fails *continuously* for 24 hours is
  disabled outright and not tried again until a human re-enables it (`webhook::breaker`). Endpoints
  are fully isolated — one cursor and one breaker each — so no receiver, slow or hostile, can affect
  another's delivery.

- **The network transport and the registration store are seams, filled by later slices.** The engine
  is generic over a `WebhookTransport` (the thing that turns a signed body into bytes on the wire)
  and holds endpoints as runtime state, so all of the above is proven here against a fake with no
  network and no database. Two pieces are deliberately **not** in this slice:
  - **The concrete TLS sender.** Delivering to an arbitrary internet endpoint needs TLS, which
    [ADR-0007](0007-in-house-vs-dependency.md) says to buy rather than hand-roll — but *which* client,
    and its `cargo-deny` and root-certificate consequences (the `webpki-roots` question
    [ADR-0031](0031-cloud-adapter-transports.md) already faced), is a dependency decision that earns
    its own ADR rather than being bolted on here.
  - **Endpoint persistence and the admin CRUD** (which URL, which secret, the cursor position) land
    in later slices; here endpoints are in-memory runtime state. *(All the deferred pieces —
    persistence, the admin CRUD, the TLS sender, and the dispatch task — have since landed; see the
    Consequences below.)*

**Rejected.**

- **An in-memory (or Redis) delivery queue** — rejected: it reintroduces the memory growth the cursor
  design exists to avoid, and Redis is already closed out of this system. The log *is* the queue.
- **No signature, or an API key in the URL/query** — rejected: an unsigned body is forgeable and a
  key in the URL leaks into the receiver's logs and referrers, and neither stops replay. HMAC over a
  bound timestamp does both.
- **An allowlist of permitted destination CIDRs** — rejected as the *primary* control: tenants
  legitimately host webhooks anywhere on the public internet, so an allowlist is either unusably
  narrow or a rubber stamp. The blocklist of never-reachable ranges plus connect-to-vetted-address is
  the right shape; an optional per-tenant allowlist can layer on later.
- **Hand-rolling TLS** for the sender (as `blob-garage` hand-rolls plaintext-signed S3) — rejected:
  TLS to arbitrary endpoints is exactly the "genuinely hard and general" thing not to build, so the
  sender waits for the client-dependency ADR rather than shipping a bespoke stack.

**Consequences.**

- `url`, `hmac`, and `sha2` join `pos-cloud`'s dependencies. All three are already in the workspace
  tree (`url` transitively, the RustCrypto pair from `blob-garage`), and the crypto is pinned to the
  same line, so no new version and no new `cargo-deny` entry appears.
- The whole engine — signing, replay window, SSRF classification, breaker, and the falls-behind
  cursor — is unit-tested with no broker, no network, and no database, including the headline
  property that a dead endpoint never advances its cursor and is suppressed by the breaker. The
  security-critical classifiers (SSRF ranges, replay window, constant-time verify) are tested against
  explicit vectors.
- Webhook bodies carry event **envelopes**, which by the PII-never-in-payload rule
  ([ADR-0026](0026-port-shapes.md), `pos-proto`) hold no personal data; a webhook is therefore not a
  channel for exfiltrating PII, which keeps it out of the data-protection blast radius even though it
  sends data off-platform.
- **Landed since:** endpoint persistence. `store-postgres` migration `0006` adds the
  `webhook_endpoints` table — `id` (a ULID PK), `tenant_id`, `store_id`, `url`, `secret`, `cursor`
  (NULL until the first delivery), `disabled` — RLS-isolated by tenant, with a `tenant_id` index for
  the admin listing and a partial index (`WHERE disabled = false`) answering the delivery task's
  enabled-load without a scan. Unlike an API-key secret (stored as a hash, [ADR-0037](0037-api-keys.md)),
  the signing secret is kept **in full**: the cloud *signs* every delivery with it, so it must be
  recoverable. `pos-cloud` fills its [`webhook::store::WebhookEndpointStore`](../../crates/pos-cloud/src/webhook/store.rs)
  seam over `PostgresWebhooks`, converting rows to and from `PersistedWebhook` / `WebhookSummary`; the
  listing is tenant-scoped and never carries the secret, while the delivery task loads the enabled
  fleet **fleet-wide as the trusted role** (RLS bypassed), the same posture as the rollup projector and
  the retention sweep. A `store-postgres` integration test proves the round-trip: register →
  tenant-scoped list → fleet-wide enabled load → advance cursor → auto-disable suppresses → scoped
  delete.
- **Landed since:** the admin CRUD, behind the super-admin session guard
  ([ADR-0034](0034-super-admin-auth.md)). `POST /admin/webhooks` SSRF-vets the destination on the
  blocking pool (a real `getaddrinfo` resolver, the same `vet` the engine uses), mints a CSPRNG id and
  signing secret at the edge, persists the endpoint, and returns the signing secret **once** — the
  tenant's copy of what the cloud signs with. `GET /admin/webhooks?tenant_id=…` lists a tenant's
  endpoints as metadata only (never the secret), and `DELETE /admin/webhooks/{id}?tenant_id=…` removes
  one within its tenant, returning `204` either way — deletion is idempotent and the tenant scope stops
  one tenant deleting another's. `CloudApp` gains the webhook-endpoint store as a seventh collaborator.
  Router tests cover register → list → delete over the fakes, and prove a loopback or plaintext URL is
  refused up front; they use IP-literal destinations so vetting needs no DNS.
- **Landed since:** the concrete TLS sender and the dispatch task that closes the loop. The
  client-dependency decision is [ADR-0038](0038-webhook-tls-sender.md) — `TlsWebhookSender`, the
  `WebhookTransport` built on the rustls/hyper stack already in the tree, dialing only the pre-vetted
  address. The **dispatch background task** ([`webhook::runner`](../../crates/pos-cloud/src/webhook/runner.rs))
  wired into `main` drives it across the fleet: each tick it loads the enabled endpoints, re-vets each
  URL, holds the live endpoints (with their cursors and **breakers**) in memory across ticks so the
  breaker windows accumulate, delivers pages after each cursor with `deliver_next`, and persists each
  cursor advance and any 24-hour auto-disable. It always runs — a cheap no-op with no registered
  endpoints — bounded per endpoint per tick so one far-behind subscriber cannot starve the fleet. The
  sweep logic is unit-tested over the fakes (deliver + persist cursor + idle; a now-unsafe URL is
  skipped, not delivered to). **The webhook feature is now complete end to end.**
