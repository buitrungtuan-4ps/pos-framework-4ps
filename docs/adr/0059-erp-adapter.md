# ADR-0059 — ERP adapter: the `ErpSink` port over a REST posting API, behind a transport seam

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-21
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) · [ADR-0038](0038-webhook-tls-sender.md) · [ADR-0058](0058-shipping-adapters.md) · [ADR-0021](0021-corrected-port-list.md)

**Context.** `docs/architecture.md` §6.1 puts ERP posting behind the `ErpSink` port: a **nightly,
whole-day** posting of revenue and consumption. Like the couriers ([ADR-0058](0058-shipping-adapters.md))
and unlike the card terminal (Track A1) or the fiscal provider (Track A2), an ERP is a REST HTTP API
and is not externally gated, so it lands in P11 as ordinary adapter work. `erp-sap` is the second
integration adapter, and the first that is *not* a courier — proving the transport-seam pattern
generalises past one port shape, which is the point of building it before the template extraction.

**Decision.**

- **The same transport-seam shape the couriers use.** `ErpTransport::request(method, path, body)` is
  the one thing that touches a socket; the concrete `TlsErpTransport` reuses the tree's pinned
  rustls/hyper stack (the `ring` provider selected explicitly, the bundled Mozilla roots). Building the
  batch body and mapping the ERP's HTTP status to a `PortError` are pure. An ERP's API base URL is
  operator configuration — one fixed, trusted host — so the adapter dials the ordinary way, **no SSRF
  surface**. A posting is created (`POST`) and read back (`GET`), so the seam carries a verb, exactly
  as ADR-0058's courier seam does. That `erp-sap` and the courier adapters share this seam almost
  character for character is deliberate: it is the input to the `templates/adapter-template` extraction
  the rule of three schedules at the **third** integration adapter, not this one.

- **The status mapping carries the port's three obligations.** Each is unit-tested and exercised by the
  shared contract suite against a **stateful stub ERP** that keys postings by `(store, business_date)`:
  - **Idempotency by revision.** `post` is idempotent by `ErpBatch::idempotency_key`
    (`store:business_date:revision`, spelled out by the port so three adapters cannot invent three
    meanings for "post this day again"). A repeat of a revision already recorded is `already_exists`,
    which a retried nightly job treats as success; the stub may equally return the same document with
    `2xx`, and the suite accepts both.
  - **A higher revision supersedes a lower one for the same day.** A late void or reprocessed day
    reposts with a higher revision, and the ERP replaces rather than accumulates — an adapter that
    appended would double-count a day's revenue, the worst failure this port has.
  - **Whole or nothing.** An account code the ERP does not know fails the *entire* batch
    `invalid_argument`, and nothing is written; half a day's revenue in an accounting period is worse
    than none, because none is visibly missing and half is not.

  HTTP→`PortError`: `2xx`→ok; on `post` `400`→`invalid_argument` (unknown account), `409`→`already_exists`
  (a revision at least this high already posted), `423`→`failed_precondition` (accounting period
  closed — a finance conversation, not a retry); on `posted` `404`→`None` (a day nothing posted for,
  which is exactly what the nightly job checks); a transport failure or any other status→`unavailable`;
  a `2xx` body that does not parse →`internal`.

- **Keyed by the trading day, not the calendar day.** The batch carries `business_date` — a bar
  closing at 02:00 posts those sales to the day it opened — and this is the opposite of `fiscal-vn`,
  which keys by calendar date, and both are right (`pos-spec.md` §4): a tax authority recognises
  calendar days, an accounting period recognises trading days. Two ADRs point the same
  revenue-skewing bug out from opposite ends.

- **Two small additive accessors on the port types.** `ErpLine` gains `kind_wire`, `amount`, and
  `quantity`, and `Quantity` gains `as_milli`, so an adapter can serialise a line without matching the
  `#[non_exhaustive]` `ErpLine` from outside its defining crate (which would force a wildcard arm that
  the workspace's denied `wildcard_enum_match_arm` lint forbids). These are read-only accessors that
  add no behaviour and no ADR-worthy surface — the exhaustive matches live in `pos-ports`, where a new
  variant is a compile error rather than a silent wildcard.

**Rejected.**

- **Real-time posting per sale.** Rejected by [ADR-0001](0001-offline-first-store-autonomy.md): an ERP
  is a system of record for accounting periods, and putting a finance system on the sales path is
  exactly the coupling the store's offline-first autonomy forbids. Nightly whole-day batches are the
  design, not a compromise.
- **Appending a correction rather than superseding.** Rejected: it double-counts a reposted day, and
  the discrepancy surfaces in a finance close weeks later with nothing left to reconstruct it from.
- **Framework-side valuation of consumption.** Rejected: costing method (FIFO, weighted average, …) is
  an accounting policy that belongs to the customer's finance function, so consumption posts as a
  quantity and the ERP values it.
- **Matching `ErpLine`'s variants in the adapter with a wildcard arm.** Rejected in favour of the
  accessors above: a wildcard over a `#[non_exhaustive]` enum is exactly the silent-drop the lint
  guards against, and a future line kind should be a compile error in `pos-ports`, not a runtime
  surprise in one adapter.

**Consequences.**

- **The port semantics are proven in the fast gate with no socket** — the shared `ErpSink` suite plus
  per-status unit tests against a stub ERP that remembers what posted. The real TLS path is exercised
  only in the gated integration lane and the soak, where the exact SAP endpoint strings, authentication,
  and chart-of-accounts vocabulary are confirmed.
- **A posting carries no personal data.** Revenue, tax, and consumption are aggregates by account and
  trading day; there is no buyer, employee, or guest in an `ErpBatch`, so unlike the courier's delivery
  contact this adapter has no PII posture to defend.
- **The retry/queue/circuit-breaker envelope is separate**, wrapping any `ErpSink` with the nightly
  dispatch wiring, the way the webhook dispatch task wraps the webhook sender — not inside this adapter.
- **The third integration adapter now has two divergent priors** — a courier (`shipping-ahamove`) and
  an ERP (`erp-sap`) — from which `templates/adapter-template` can generalise the transport seam, the
  status-mapping shape, and the stub-driven contract test.
