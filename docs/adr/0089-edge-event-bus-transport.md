# ADR-0089 — The edge reaches the event bus directly, over TLS on its own port

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-02
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) (the store is outbound-only) · [ADR-0031](0031-cloud-adapter-transports.md) (hand-rolled transports, no SDKs) · [ADR-0044](0044-fork-and-deploy.md) (what runs on the VPS) · [ADR-0087](0087-edge-relay-and-event-publish.md) (the outbox publish this transports) · `docs/roadmap-v3.md` (slice E7, debates D23/D25)

> **Correction, found while implementing (2026-09-02): the URL does not carry the token.** This
> record says `POS_EDGE_NATS_URL` "already carries it", and that `link-nats` therefore needs no
> change. Both are wrong. `async-nats` 0.50 — the pinned version — builds its `CONNECT` frame from
> `ConnectOptions::auth` only; `ServerAddr::username`, `password` and `has_user_pass` are public
> accessors with **no caller inside the crate**. A token in the URL is silently discarded, and the
> `authorization { token: … }` block `bootstrap.sh` generates then refuses the connection. The same
> mistake is in `bootstrap.sh`'s own instructions for arming the cloud's ingest cursor, so the
> documented way to turn that feed on could not work either.
>
> It survived because the integration suite runs NATS with **no authorization at all** and connects
> with a bare `127.0.0.1:4222` — the one posture no deployment uses. The fix is
> `link-nats`'s `endpoint::split`, which lifts the credentials out of the URL and presents them
> through `ConnectOptions`; the URL keeps its documented shape, and the secret stays in the one
> mode-0600 file, which is the reason this record gave for putting it there. So the consequence
> below should read: **no change in `pos-edge` or `pos-cloud`, and one contained change in
> `link-nats`.** The transport decision itself is unaffected.

**Context.** E3 built the edge's outbox publish and wired it into `serve()`: `EventPublisher` drains
the store's committed events over `MessageLink`, `link-nats` implements that link over JetStream, and
`POS_EDGE_NATS_URL` names the broker. Every part of it is live code on a live path.

It has never reached a broker. `deploy/compose.yml` puts `nats` on the `backend` network with
`internal: true` and publishes no port; the only service publishing anything is Caddy (`80`, `443`).
There is no proxy route for NATS either. So `POS_EDGE_NATS_URL` has **no valid value**: a store cannot
reach the broker from outside the box, and there is no address to give it.

The failure is silent, which is why it survived. `spawn_event_publish` treats an unreachable broker as
non-fatal on purpose — the store keeps trading and the outbox stays durable — so a fleet in this state
looks healthy from the till and empty from the cloud. Rollups, revenue reports, X/Z aggregation and
reconciliation all read nothing, and nothing says why.

Two transports were considered seriously: publishing NATS's own port, or carrying NATS over WebSocket
through the existing HTTPS ingress. **They are performance-indistinguishable for this workload.** The
publisher drains in batches every 5 s; at a store's peak of 27 events/s that is ~135 events, roughly
40–100 KB, so ~20 KB/s per store. WebSocket's per-frame header and its mandatory client-side masking
XOR are a rounding error at that rate, and both paths pay the same TLS cost. Throughput does not
decide this.

**Decision.**

- **The `nats` container publishes `4222` and the edge connects to it directly.** Caddy is not in the
  path, so this needs no reverse-proxy TCP plugin and no change to the image built for either TLS
  path. `link-nats` needs no change either: `link.rs` passes the URL straight to
  `async_nats::connect`.

- **The port never opens without TLS.** Publishing `4222` makes the broker **internet-facing** — there
  is no proxy, no Cloudflare, and no firewall in front of it by default. Its TLS and its token are
  then the *only* thing protecting the fleet's event stream. So server TLS and the published port land
  in the same change, never one before the other, and the runbook says to restrict the port at the
  host firewall to the addresses stores actually dial from where that is knowable.

- **Token authentication first; mTLS is a later slice.** `bootstrap.sh` already generates a 32-byte
  token into `nats.conf`, and the URL carries it — through `link-nats`, which has to lift it into
  the connect options itself (see the correction above; `async-nats` reads credentials only from
  those). Carrying it in the URL is precisely why `POS_EDGE_NATS_URL` lives in the environment file
  and not in `config.toml`. That gets the first store publishing. Per-store client certificates follow as their own slice, because they change
  provisioning — every store needs a certificate issued at activation — and that is a bigger change
  than making the bus reachable.

- **When mTLS lands, the client CA is private and ours** ([D25](../roadmap-v3.md)). The *server*
  certificate may be public; the CA that verifies *store* certificates must not be, because
  configuring a public CA there means anyone who can obtain a certificate from it can speak to the
  bus — mTLS turned into a no-op that still looks configured. Store certificates carry the `store_id`
  as their subject and NATS maps it (`verify_and_map`), which is what finally gives a box an identity
  it proves rather than one derived from its store id.

- **The CA key starts on the box, and that is recorded as a pilot posture.** Generating it in
  `bootstrap.sh` keeps adding a store automatic; the cost is that owning the VPS becomes owning every
  store's identity, which is the same reasoning [D1](../roadmap-v3.md) applied to release signing.
  The pilot accepts it; a fleet moves the CA offline before scale. The runbook states which posture a
  deployment is in, so nobody has to infer it from a file listing.

**Why not the reverse proxy.** Four things a proxy cannot give, none of them throughput:

1. **Client certificates.** TLS must terminate *at* NATS for it to authenticate a store by
   certificate. Behind a proxy, TLS terminates at the proxy and the client's identity dies there — a
   proxy can put it in a header, and NATS does not read headers. This is the one that decided it,
   because per-store identity on the bus is a gap this program has already flagged.
2. **Cluster topology.** NATS gossips its cluster to clients, which then failover between nodes on
   their own. Through a proxy those advertised addresses are internal ones the store cannot reach, so
   discovery breaks and the proxy has to become the load balancer.
3. **Blast radius.** Caddy runs on `cpus: 0.25` / `mem_limit: 96m` and already carries every store's
   config long-poll. Putting the bus through it doubles its persistent connections at the 500-store
   target and makes one process the single failure for both the console and the event stream.
4. **Reconnect cost.** A WebSocket adds an HTTP upgrade round-trip per reconnect. Irrelevant to a 5 s
   batch loop, and this reason has since weakened: it leaned on the relay live mode roadmap v3 was
   deferring, and [ADR-0062](0062-the-relay-wake.md) declined that mode. Reasons 1–3 are what carry
   the decision now.

**Revisit triggers.** This decision is cheap to reverse — the transport is a URL, and
`async_nats::connect` accepts either scheme from the same binary (`websockets` is in async-nats'
default features, so WebSocket support is already compiled in). Revisit if **any** of these becomes
true:

- Per-store mTLS is abandoned, which removes the reason that decided it.
- A store network blocks outbound `4222` and cannot be changed — a mall or corporate LAN that permits
  only `443`. Then WebSocket over the existing ingress is strictly better, and it is a config change.
- Caddy is measured, not assumed, to be the constraint at the target fleet size and consolidating on
  one ingress would help.
- The bus starts carrying large payloads — the log tail [ADR-0078](0078-sync-and-ota-closure.md)
  defers — where the extra proxy hop and the masking pass stop being free.

**Deliberately deferred (flagged, not silently dropped).**

- **Per-store mTLS**, as above: its own slice, and the point of choosing this transport.
- **Whether `internal: true` permits a published port.** Docker's `internal` flag removes a network's
  route *out*; published ports are host→container DNAT, which should be unaffected. Should is not
  verified, and it cannot be verified from a repository. The implementation slice proves reachability
  on a real box before the runbook claims it works, and falls back to attaching `nats` to the
  `frontend` network if not — at the cost of giving the broker egress it does not need.
- **Certificate rotation for NATS** — and, because of it, **this ADR's implementation is sequenced
  after the TLS-posture one**. The decision above binds TLS and the published port into one change but
  does not say where NATS's server certificate comes from, and on the ACME paths there is only one
  real certificate on the box: the one Caddy renews inside its own `caddy_data` volume. That
  certificate is *valid* for the broker — same hostname, same machine, a different port — so the
  question is only plumbing and renewal. Both answers are bad:
  - Mounting `caddy_data` into the NATS container couples the broker to another service's private
    storage layout (the path embeds the ACME directory URL), and
  - Caddy renews roughly 30 days before expiry, so a NATS that does not reload keeps serving the old
    certificate until it expires and then **every store fails TLS and the bus goes dark silently** —
    publish failure is non-fatal by design, so nothing complains for 60–90 days. That is the exact
    failure class this program keeps finding, engineered in deliberately.

  So the certificate's location must be a **configured** path that some posture put there on purpose,
  not a reach into a neighbour's volume. Debate D24's `TLS_MODE` work lands first and establishes that
  path for all four postures — ACME, Cloudflare DNS-01, a brought certificate, and termination
  upstream — and E7's implementation then points at it. This ADR's transport decision is unaffected by
  that ordering; only its delivery date is.
- **Restricting the port by source address.** Stores on residential or mobile connections have no
  stable address, so a general allow-list is not available; per-deployment firewalling is a runbook
  step, not something the compose file can decide.

**Rejected.**

- **NATS over WebSocket through Caddy.** The better choice on firewall traversal and cheaper on
  operations, and it needs no new dependency on either side. Rejected because it forecloses client
  certificates, which is the property being bought here. Kept as the documented fallback with its
  trigger named above rather than dismissed.
- **Replacing the bus with HTTPS `POST /internal/ingest`.** Fewest moving parts on paper, but it does
  not remove NATS — the cloud's own projector still consumes from it — so the dependency stays and the
  edge just stops using it. In exchange the durable at-least-once semantics JetStream provides for
  free would have to be re-implemented over HTTP, and `/internal/ingest` has no authentication at all
  until O5. A real cost for an apparent simplification.
- **A TCP-proxying reverse proxy.** Stock Caddy cannot proxy raw TCP; the `caddy-l4` module can, which
  means building a custom Caddy image on *both* TLS paths — including the one that currently uses the
  stock image. Publishing the container's own port achieves the same thing with nothing added.
- **Sharing Caddy's ACME certificate into NATS by mounting `caddy_data`.** Works, and depends on the
  private layout of another service's volume plus a renewal hook. Deferred above rather than adopted.

**Consequences.**

- **The outbox stops publishing into nothing.** With this slice the cloud finally receives store
  events, which is what rollups, revenue and product-mix reporting, X/Z aggregation and reconciliation
  have all been reading empty. It is the fifth "written but never wired" gap this program has closed.
- **The VPS gains one internet-facing port**, protected by TLS and a token and nothing else. That is
  the honest cost of this choice and the reason TLS is not optional here.
- **No code change in `pos-edge` or `pos-cloud`**, and one contained change in `link-nats` — the
  credential lift the correction above describes, which this record originally said was unnecessary.
  The rest is deployment and configuration: `compose.yml`, `nats.conf`, `bootstrap.sh`, and the
  runbooks.
- **No wire, protocol, or `pos-proto` change**, and no migration.
- **An operator gains one provisioning value**: `POS_EDGE_NATS_URL` in the store's environment file,
  which the new-store wizard already emits as a commented line ready to fill in.
