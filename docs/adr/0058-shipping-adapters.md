# ADR-0058 — Shipping adapters: the `ShippingDispatch` port over a REST courier API, behind a transport seam

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-21
**Relates to** [ADR-0038](0038-webhook-tls-sender.md) · [ADR-0054](0054-edge-cloud-http-client.md) · [ADR-0021](0021-corrected-port-list.md)

**Context.** `docs/architecture.md` §6.1 names two couriers — Ahamove and Grab Express — behind the
`ShippingDispatch` port, and gives the port three operations (create, cancel, track) plus the rule
that **a courier's status becomes a domain event**. Unlike the card terminal (Track A1, whose
transport — TCP, serial, or a Windows-only DLL — is an open architectural question) or the ShopeeFood
channel (Track A3, direct-vs-aggregator), a courier is unambiguously a **REST HTTP API**, so it is not
externally gated and lands in P11 as ordinary adapter work. This ADR fixes how a courier adapter is
shaped, so the second and third are transcription rather than redesign.

**Decision.**

- **The socket lives behind a per-adapter transport seam; everything else is pure.** Exactly as the
  webhook sender ([ADR-0038](0038-webhook-tls-sender.md)) and the edge→cloud client
  ([ADR-0054](0054-edge-cloud-http-client.md)) are built, a `CourierTransport` trait is the one thing
  that touches a socket, and the concrete `TlsCourierTransport` reuses the tree's pinned rustls/hyper
  stack (the `ring` provider selected explicitly, the bundled Mozilla roots). Building the request,
  mapping the courier's status vocabulary, and mapping the courier's HTTP status to a `PortError` are
  all pure functions. A courier's API base URL is operator configuration — one fixed, trusted host —
  so, like the edge→cloud client, the adapter dials the ordinary way with **no SSRF surface** to
  defend (the webhook sender's SSRF guard exists because *its* destinations are tenant-supplied).

- **The seam carries an HTTP verb.** A courier job is a REST resource: booking is a `POST` to the
  shipments collection, cancelling is a `POST` to the job's cancel sub-path, and tracking is a `GET` of
  the job. So `CourierTransport::request(method, path, body)` names the verb rather than fixing `POST`
  as the edge→cloud seam does — the one deliberate difference from ADR-0054, because a `GET` track that
  masqueraded as a `POST` would be a lie in the wire the integration lane checks against the real API.

- **The status mapping is the load-bearing part, and it is what the fast gate proves.** The port's
  contract turns on four mappings, each unit-tested and exercised by the shared contract suite against
  a **stateful stub courier** that speaks the wire:
  - booking is **idempotent by `shipment_id`** (sent as the courier's idempotency key) — a retry after
    a timeout returns the same job, never a second rider the store pays for twice;
  - a cancel of a **completed** job is `failed_precondition`, not a quiet success — a successful-looking
    cancel leaves the store expecting a refund it will not get;
  - a cancel of an **already-cancelled** job succeeds, because cancellation is retried;
  - an **unknown** reference is `not_found`, and a finished job is still **trackable**, which is how a
    missed callback is reconciled.

  HTTP→`PortError`: `2xx`→ok; on booking `400`→`invalid_argument` (unresolvable address) and
  `409`→`resource_exhausted` (no rider — a business outcome the caller surfaces, not a blind retry);
  on cancel `404`→`not_found` and `409`→`failed_precondition`; on track `404`→`not_found`; a transport
  failure or any other status →`unavailable`; a `2xx` body that does not parse, or that echoes a
  non-ULID id / unknown currency, →`internal` (the courier breaking its own contract).

- **A courier status this build does not recognise is preserved as *unrecognised*, never coerced.** The
  courier's vocabulary (`ACCEPTED`, `IN PROCESS`, `COMPLETED`, `CANCELLED`, …) maps onto
  `ShipmentStatus`, and anything unmapped becomes an unrecognised `Open<ShipmentStatus>` — which
  `ShipmentUpdate::is_terminal` reports non-terminal, the safe direction: a job wrongly believed live
  costs one more poll; one wrongly believed finished stops being tracked.

- **Callbacks do not come back through this trait.** A courier's webhook lands on `pos_cloud`'s HTTP
  surface, is verified there, and becomes a domain event; the port already documents that a method
  waiting for someone to call in is a server, not a port. `track` is the polling path that recovers a
  missed callback, and returns the same `Shipment` shape either way.

- **The exact courier strings are pinned in the gated integration lane.** The concrete endpoint paths,
  authentication headers, and status tokens in the adapter are this adapter's own mapping; the real
  Ahamove/Grab Express strings are confirmed against the live API in the gated lane and the soak — the
  same split ADR-0038 drew between the provable request-shaping (fast gate, no socket) and the real TLS
  path. What the fast gate proves is the port *semantics*, which do not change when a field is renamed.

- **Extract `templates/adapter-template` at the third integration adapter, not before.** `docs/roadmap.md`
  P11 makes the rule of three explicit. Ahamove is the first courier and Grab Express the second; the
  duplicated TLS-transport boilerplate between them is the evidence, not a smell to pre-empt. The
  template is extracted when the third integration adapter (`erp-sap`) is written.

**Rejected.**

- **One shared HTTP client crate for every adapter, built now.** Rejected as premature: the rule of
  three ([roadmap P11](../roadmap.md)) says extract the template from three real adapters, not design
  it from one. The near-identical `TlsCourierTransport` and `TlsHttpTransport` are the input to that
  extraction.
- **Tunnelling `GET` track through a `POST`** to keep the seam identical to ADR-0054 — rejected: it
  would make the wire the integration lane validates disagree with the real API for no benefit.
- **Letting the adapter receive callbacks** — rejected by the port itself: callbacks are a server
  surface on `pos_cloud`, and routing them back through a client trait would create a second code path
  that could disagree with the polled one.
- **Coercing an unknown courier status to the nearest known one** — rejected: guessing `Cancelled` or
  `Completed` for a status this build predates is exactly the mistake that stops a live job being
  tracked.

**Consequences.**

- **The port semantics are proven in the fast gate with no socket**, for every courier adapter: the
  shared `ShippingDispatch` suite plus per-status unit tests run against a stub courier that remembers
  its jobs. The real TLS path is exercised only in the gated integration lane and the soak.
- **The delivery contact is personal data, and this is where it leaves the system.** The booking
  request transmits the recipient's name, phone, and address to the courier, which is a **data
  processor** under Vietnam's PDPD (Decree 13/2023) — lawful basis: performance of the delivery
  contract; a processor agreement with the courier and the roadmap A6 posture apply. The adapter
  transmits it for that sole purpose, **never logs it** (the port's `DeliveryContact` has only a
  redacting `Debug`), and the tracked `Shipment` never carries it back. This is domestic processing;
  any change that routed a courier's API off-shore would be a cross-border transfer needing its own
  legal basis and is out of scope here.
- **The retry/queue/circuit-breaker envelope is separate.** Per-adapter queue, error mailbox, and
  circuit breaker (roadmap P11) wrap *any* `ShippingDispatch` and land with the dispatch wiring, the
  way the webhook dispatch task wraps the webhook sender — not inside this adapter.
- **A second country's courier is a config value, not a code change**, because the port is
  country-neutral; a new courier is a new adapter behind the same seam.
