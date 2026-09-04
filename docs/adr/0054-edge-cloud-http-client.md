# ADR-0054 — The edge→cloud HTTP client reuses the tree's rustls stack, behind a transport seam

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-04
**Relates to** [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0038](0038-webhook-tls-sender.md) · [ADR-0047](0047-minisign-verification.md) · [ADR-0048](0048-ota-rollout-model.md) · [ADR-0050](0050-activation-code-exchange.md) · [ADR-0053](0053-cloud-sync-port.md) · [ADR-0088](0088-ota-artifact-hosting.md) (Amendment 1 corrects the artifact path pinned here) · [ADR-0097](0097-internal-route-authentication.md)

**Context.** [ADR-0053](0053-cloud-sync-port.md) added the `CloudSync` port — the store's one
request/response channel to the cloud, carrying `activate(code)` (the first-boot activation exchange,
[ADR-0050](0050-activation-code-exchange.md)) and `fetch_update(release)` (the OTA artifact,
[ADR-0048](0048-ota-rollout-model.md)) — and deferred the concrete adapter. This is that adapter:
`cloud-sync-http`, the client the edge composes to reach its cloud.

Unlike the webhook sender ([ADR-0038](0038-webhook-tls-sender.md)), which dials *arbitrary tenant
endpoints* and therefore had to close a DNS-rebinding hole, this client dials exactly one host: the
store's own cloud, whose base URL is configuration the operator sets at activation. There is no SSRF
surface — the destination is trusted infrastructure, not attacker-influenced input — so the client
resolves and connects the ordinary way. What it must get right instead is small and load-bearing: the
two request shapes, and the mapping from the cloud's HTTP status to the *right*
[`PortError`](../../crates/pos-ports/src/error.rs) status, because a caller branches on that status and
a wrong one is a wrong retry policy (the same obligation the `CloudSync` contract suite pins).

**Decision.**

- **Build on the rustls/hyper stack already in the tree, exactly as [ADR-0038](0038-webhook-tls-sender.md)
  did.** The adapter adds direct-dependency lines on crates already present at the versions the cloud
  pins — `hyper` (client, http1), `hyper-util`, `http-body-util`, `bytes`, `tokio-rustls`
  (`default-features = false`, `ring`/`tls12`/`logging`), `webpki-roots`, `url` — so **no new crate, no
  new version, and no new `cargo-deny` entry** enters the graph. The `ring` provider is pinned
  explicitly so feature unification can never pull `aws-lc-rs`; roots come from the bundled
  `webpki-roots` Mozilla set, hermetic for a fork-and-deploy edge. This is the buy-what-is-already-bought
  posture [ADR-0007](0007-in-house-vs-dependency.md) and [ADR-0038](0038-webhook-tls-sender.md) fix.

- **Put the socket behind an `HttpTransport` seam; keep everything else pure.** The adapter is
  `HttpCloudSync<T: HttpTransport>`: it builds the request body (pure), calls `transport.request(...)`,
  and maps the `(status, body)` into an `ActivationGrant` / artifact bytes / `PortError` (pure). The
  production `T` is `TlsHttpTransport` — parse the base URL, dial `host:port`, rustls-handshake against
  the hostname, one HTTP/1.1 request, read the full body, all bounded by a `tokio::time::timeout`. This
  is the same seam-and-pure-core split [ADR-0038](0038-webhook-tls-sender.md) drew, and it is what lets
  the `CloudSync` contract suite run in the ten-minute pull-request gate (below).

- **Both calls are `POST` with a JSON request body.** `activate` posts `{"code": "…"}` to
  `POST {base}/activate` — the exact route [ADR-0050](0050-activation-code-exchange.md) built — and
  reads `{"device_id", "credential"}` back. `fetch_update` posts `{"release": "…"}` to
  `POST {base}/internal/ota/artifact` and reads the raw signed artifact bytes back (an
  `application/octet-stream` body, not JSON). Posting the release tag in a JSON body — rather than
  interpolating a free-text [`ReleaseTag`](../../crates/pos-proto/src/text.rs) into the URL path —
  sidesteps hand-rolled percent-encoding, which is precisely the "general and fiddly" work
  [ADR-0007](0007-in-house-vs-dependency.md) says not to hand-roll; `serde_json` already escapes it.
  The artifact endpoint sits under `/internal/` beside `/internal/ingest` and `/internal/reconcile`
  — the store-facing surface, not the public `/v1` — and is served by the cloud OTA publisher in
  P9e-4; this adapter is its client and the contract suite pins the wire both sides speak.

- **Status → `PortError`, no oracle.** `activate`: `2xx` → parse the grant (a malformed or empty body
  is `internal`, an unparseable `device_id` is `internal` — the cloud broke its own contract);
  `400` → `invalid_argument` (the code was malformed); `403` → `permission_denied` (refused — spent,
  revoked, and unknown collapse to this one status, the no-oracle posture of
  [ADR-0050](0050-activation-code-exchange.md)); anything else, including `503`, → `unavailable` (retry).
  `fetch_update`: `200` → the bytes; `404` → `not_found` (so the caller installs nothing, never empty
  bytes); anything else → `unavailable`. A transport failure (dial, handshake, timeout) is always
  `unavailable`.

- **The bytes are not trusted here.** `fetch_update` returns the artifact *as received*. The transport
  is not a trust boundary ([ADR-0053](0053-cloud-sync-port.md)): the caller verifies the minisign
  signature with the `Signer` ([ADR-0047](0047-minisign-verification.md)) before staging, so this
  adapter neither verifies nor needs to — a spoofed cloud cannot make it install code, only fail
  verification downstream.

**Rejected.**

- **`reqwest`** — rejected for the same reasons [ADR-0038](0038-webhook-tls-sender.md) rejected it: a
  new subtree and a default `aws-lc-rs` provider, where the in-tree hyper wiring is more code but zero
  new supply chain.
- **A `GET` with the release in the path** (`GET /internal/ota/artifact/{release}`) — rejected: a
  free-text `ReleaseTag` in a path segment needs percent-encoding, and hand-rolling that (or pulling a
  `percent-encoding` crate for one call site) is more surface than a JSON body that `serde_json`
  already escapes. `POST`-with-body also makes the two calls symmetric behind one transport method.
- **Reusing `MessageLink`'s transport** (`link-nats`) — rejected by [ADR-0053](0053-cloud-sync-port.md)
  already: `MessageLink` is deliberately outbound-only fire-and-forget, and request/response over a
  broker would reintroduce exactly the cloud-round-trip coupling the offline-first design forbids on
  that path.
- **An always-on real-socket contract test in the pull-request gate** — rejected. A live handshake
  needs an HTTPS peer (or a self-signed cert and a bespoke loopback server, a new `rcgen`-shaped
  subtree). The pure request/response behaviour is what the suite asserts, so it runs against a stub
  transport in the fast gate; the real socket is the gated integration lane and the soak, the split
  [ADR-0038](0038-webhook-tls-sender.md) and [ADR-0031](0031-cloud-adapter-transports.md) already draw.

**Consequences.**

- **No new `cargo-deny` entry.** Every crate the adapter names is already in `Cargo.lock` at the pinned
  version via the cloud's webhook sender ([ADR-0038](0038-webhook-tls-sender.md)) and `async-nats`
  ([ADR-0031](0031-cloud-adapter-transports.md)); the `webpki-roots` licence exception already covers
  the root set. The adapter adds direct-dependency lines, not crates.
- **The `CloudSync` contract suite runs in the pull-request `test` job.** `HttpCloudSync` over a stub
  transport that reproduces the cloud's exact status/body responses passes `cloud_sync_suite!`, so the
  request-shaping and status→`PortError` mapping — the adapter's whole branching behaviour — is checked
  on every pull request, in milliseconds, with no socket. The real TLS path (`TlsHttpTransport`) is
  exercised in the gated integration lane against a live cloud and in the soak, not the ten-minute gate.
- **Nothing is foreclosed.** The `HttpTransport` seam means swapping the hand-wired hyper client for
  another, or the bundled roots for the OS store, changes one file. The cloud's `/internal/ota/artifact`
  route is defined by this ADR and implemented by its P9e-4 counterpart; until then the artifact path
  has a client and a pinned contract but no server, which is the ordinary order in which a
  request/response pair lands here.

**Correction (2026-09-04) — `fetch_update`'s pinned path is wrong, and this ADR's reason for it no longer holds.**

Above, the artifact endpoint is pinned at `POST {base}/internal/ota/artifact`, justified as sitting
"under `/internal/` beside `/internal/ingest` and `/internal/reconcile` — the store-facing surface, not
the public `/v1`".

**`/internal/` is not the store-facing surface.** It became the cloud's own *trusted-network* surface
when the proxy was taught to deny the whole prefix (`deploy/Caddyfile.d/site.caddy`, mirrored in
`k8s/pos-cloud.yaml`), closing a real hole: three `/internal` handlers were reachable and
unauthenticated from the internet. The deny answers `404` to every `/internal/*` request from outside
the box, and a store reaches its cloud through exactly that proxy — so the path pinned here cannot be
called by the client this ADR describes.

The store-facing surface is `/sync/stores/{store_id}/…`, where the cloud resolves the tenant from the
scoped key rather than trusting a body field. [ADR-0088](0088-ota-artifact-hosting.md) Amendment 1 moves
the artifact route there and states the rule; [ADR-0097](0097-internal-route-authentication.md) had
already reached it for the sibling `/internal/ota/report`.

Nothing else in this ADR changes: the transport seam, the rustls reuse, the `POST`-with-JSON-body shape
and the reasons for each all stand. What changes is the one path constant, and the contract suite that
pins it — in R2's implementation slice, not here.
