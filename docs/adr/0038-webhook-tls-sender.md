# ADR-0038 — The webhook TLS sender reuses the tree's rustls stack, and owns its dial

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0007](0007-in-house-vs-dependency.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0031](0031-cloud-adapter-transports.md) · [ADR-0032](0032-webhooks.md)

**Context.** [ADR-0032](0032-webhooks.md) built the webhook delivery engine against a
`WebhookTransport` seam and deferred the concrete sender, because delivering to an arbitrary internet
endpoint needs TLS — which [ADR-0007](0007-in-house-vs-dependency.md) says to *buy*, not hand-roll —
and *which* TLS client, with what root-certificate story and what `cargo-deny` fallout, is a
dependency decision that earns its own ADR. This is that decision.

The sender is not an ordinary HTTP client call, because of the one hard requirement
[ADR-0032](0032-webhooks.md) fixed: **the transport must connect only to the addresses the SSRF vet
already approved, and must never re-resolve the hostname** — re-resolving between the check and the
connect is the DNS-rebinding hole the whole SSRF design exists to close. So the client must let us
choose the connection address while still doing TLS (SNI, certificate verification) and the `Host`
header against the *hostname*. That requirement, not raw HTTP, drives the choice.

**Decision.**

- **Build the sender on the rustls/hyper stack already in the tree, not a new HTTP-client crate.**
  The cloud already links, and `cargo-deny` already reviews, every crate the sender needs:
  `hyper` + `hyper-util` (the HTTP/1.1 client, via `axum`) and `rustls` + `tokio-rustls` +
  `webpki-roots` + `ring` (the TLS stack, via `async-nats`, [ADR-0031](0031-cloud-adapter-transports.md)).
  The sender adds **direct-dependency lines on crates already present at the same versions** —
  `hyper` (client, http1), `hyper-util`, `http-body-util`, `bytes`, `tokio-rustls`, `webpki-roots` —
  so no new crate, no new version, and no new `cargo-deny` entry enters the graph. This is the same
  posture `blob-garage` took in reverse: it hand-rolled plaintext HTTP to *avoid* an SDK subtree; here
  the TLS is genuinely hard so we buy it, but we buy what is already bought rather than pulling a
  parallel stack.

- **Own the dial; do not delegate address selection to the client.** For each delivery the transport
  opens a `TcpStream` to one of the endpoint's **pre-vetted** addresses at the URL's port (443 for
  `https`), performs the rustls handshake with the server name set to the URL's *host* (so SNI and
  certificate verification bind to the hostname, not the IP), wraps the TLS stream for `hyper`, and
  sends exactly one HTTP/1.1 `POST` — the signed body, with the `Host`, content-type, and the two
  [ADR-0032](0032-webhooks.md) signature headers. One request per connection: webhooks are infrequent
  and independent, so a fresh connection per delivery is the simplest correct thing and needs no
  connection pool or idle-eviction. Because the address comes from the already-vetted set and the host
  is never re-resolved, the rebinding gap is closed *by construction* — the transport cannot connect
  anywhere the vet did not approve.

- **Pin the `ring` crypto provider explicitly, and trust the bundled Mozilla roots.** The
  `ClientConfig` is built `with_provider(ring)` rather than relying on the ambient default, so the
  sender can never silently pull `aws-lc-rs` (a heavy C-build provider absent from the tree) through
  feature unification — `tokio-rustls` is depended on with `default-features = false` and only the
  `ring`/`tls12`/`logging` features, matching the resolution `async-nats` already produces. Roots come
  from **`webpki-roots`** (Mozilla's bundled CA set), not the OS trust store: the cloud is a
  fork-and-deploy container (`docs/roadmap.md` P8), so a bundled, reproducible root set that needs no
  `ca-certificates` package in the base image is the right default — and `webpki-roots` is *already*
  in the tree and already an admitted, reviewed `deny.toml` exception
  ([ADR-0031](0031-cloud-adapter-transports.md)), so trusting it here costs nothing new.

- **Every delivery is bounded by a timeout.** The whole connect → handshake → send → response is
  wrapped in a single `tokio::time::timeout`, so a black-hole endpoint cannot wedge the dispatch loop;
  a timeout is an ordinary failed delivery — the breaker backs off and the cursor does not advance,
  exactly as [ADR-0032](0032-webhooks.md) specifies. Any non-2xx response, TLS error, or connection
  failure is likewise a plain `DeliveryError`; the engine already treats every failure identically.

**Rejected.**

- **`reqwest`** — rejected. It is the ergonomic choice and its `.resolve()` override *can* pin the
  address, but it is a new subtree (its own connector, `tower`, encoding stack) the tree does not
  carry, and its `rustls-tls` feature defaults to the `aws-lc-rs` provider on current rustls — pulling
  exactly the heavy C-build crypto crate this codebase has so far kept out. Steering reqwest onto the
  in-tree `ring` provider is possible but fiddly and version-fragile, and the connect-to-vetted-address
  requirement is *safer* when we own the dial than when it rides on a resolver-override the library
  applies. The lean hyper wiring is more code but zero new supply chain, which is the trade this
  project consistently makes ([ADR-0007](0007-in-house-vs-dependency.md), [ADR-0031](0031-cloud-adapter-transports.md)).
- **Hand-rolling TLS** (as `blob-garage` hand-rolls plaintext-signed S3) — rejected by
  [ADR-0032](0032-webhooks.md) already: TLS to arbitrary endpoints is the "genuinely hard and general"
  thing to buy. We buy rustls; we only own the TCP dial and the one-request wiring around it.
- **Trusting the OS certificate store** (`rustls-native-certs`, also in the tree) — rejected as the
  default: it ties the cloud's trust to whatever cert bundle the base image ships, which a
  fork-and-deploy operator does not control or audit. The bundled roots are hermetic. (An operator who
  wants the OS store can flip it later behind the same seam — this is not foreclosed.)

**Consequences.**

- **No new `cargo-deny` entry.** `hyper`, `hyper-util`, `http-body-util`, `bytes`, `tokio-rustls`,
  `rustls`, `webpki-roots`, and `ring` are all already in `Cargo.lock` at the versions the sender
  pins; the `webpki-roots` licence exception and the `skip-tree = async-nats` collapse that
  [ADR-0031](0031-cloud-adapter-transports.md) added already cover this stack. The sender adds
  direct-dependency lines, not crates.
- **The wire is proven at its seams in-process; the socket path is gated.** The address/host/port/
  request-target derivation from a `VettedUrl` and the header assembly are a pure function
  (`transport::prepare`) unit-tested in the ordinary `test` job — method, `Host`, content-type, the
  two signature headers, the origin-form request target, and the `ip:port` set the dial will use. The
  actual TLS handshake needs a live HTTPS peer, so an end-to-end proof belongs to the gated
  integration lane and the soak, not the ten-minute pull-request gate — the same split
  [ADR-0031](0031-cloud-adapter-transports.md) drew for `blob-garage`'s real-S3 suite.
- **Nothing is foreclosed.** The sender stays behind the `WebhookTransport` seam, so swapping the
  hand-wired hyper client for `reqwest`, or the bundled roots for the OS store, changes one file and
  nothing that depends on it.
