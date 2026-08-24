# ADR-0056 — Public order intake is `POST /v1/orders` over the OrderIn port, tenant-bound at the edge of the request

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-21
**Relates to** [ADR-0012](0012-qr-ordering-via-cloud.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0037](0037-api-keys.md) · [ADR-0019](0019-openapi-generation.md)

**Context.** `docs/roadmap.md` P11 builds the `OrderIn` port's callers, and it says to build the
public `POST /v1/orders` path *first* because marketplaces, QR ordering
([ADR-0012](0012-qr-ordering-via-cloud.md)) and the public API all reuse it — the shared intake is
what makes QR ordering nearly free. `OrderIn` is a *driving* port ([ADR-0026](0026-port-shapes.md) §5):
the application implements it and the endpoint calls in. Its contract — idempotency keyed on
`(sales_channel, external_reference)`, store-price-wins with `repriced` reported, unknown item refused
not substituted, guest note never in the log, and **acceptance never requiring the cloud** — is
already fixed on the port and proven by its suite. This ADR fixes the *public HTTP surface* over it.

Two things the port does not carry, and the endpoint must supply, drive the decision:

1. `InboundOrder` names a `store_id` but no tenant. A `/v1` key is tenant-scoped
   ([ADR-0037](0037-api-keys.md)), so the endpoint has to bind the two — otherwise one tenant's key
   could place orders into another tenant's store, the single worst multi-tenant failure. There is no
   store→tenant lookup at this layer today.
2. The real implementor of `OrderIn` is the *store's edge* (it reprices against its own menu, routes
   to the kitchen, and must accept offline). The cloud endpoint is a *caller* that relays to the
   store; it does not itself implement the port.

**Decision.**

- **`POST /v1/orders`, bearer-authed with a new `Scope::PlaceOrders`.** Order submission is a write and
  a distinct capability from the read scopes, so it is its own scope, deny-by-default like the rest
  ([ADR-0037](0037-api-keys.md)). Authenticate → require `PlaceOrders` → only then touch the order.

- **Bind the store to the caller's tenant through a `StoreDirectory` seam, before submitting.** The
  handler asks `StoreDirectory` which tenant owns the request's `store_id`; if that is not the grant's
  tenant — or the store is unknown — it answers a single generic `404`, with no oracle distinguishing
  "not yours" from "not real". This is the isolation boundary, checked at the edge of the request
  exactly as the rollup read checks `grant.tenant()` before touching data. The seam is one method
  (`tenant_of(store) -> Option<TenantId>`); the config tree, which already holds the
  Tenant→Brand→Store hierarchy, backs it in the binary, and a fake backs it in tests.

- **The endpoint is generic over `OrderIn`; it does not implement it.** The handler maps the JSON
  request to an `InboundOrder`, calls `submit`, and maps the `OrderAcceptance` (or `PortError`) to
  HTTP. In the binary the `OrderIn` it calls is the cloud→store relay; in tests it is `FakeIntake`. The
  full `OrderIn` contract — idempotency, reprice, unknown-item refusal, QR staff-confirmation — is the
  implementation's, surfaced here unchanged: a repeat is `200` with `created:false`, a first accept is
  `201`, an unknown item is `400`, a closed store / unknown table / disabled capability is `409`, a
  rate-limit is `429`, and a genuine conflicting reuse of a reference is `409`.

- **A dedicated request DTO, not the wire `InboundOrder`.** `pos-proto` carries no `utoipa`, so the
  domain types are not `ToSchema`; and `Money`/`Quantity`/ids have their own compact wire forms. The
  endpoint owns a small `OrderRequest`/`OrderResponse` pair with `ToSchema`, mapping to and from the
  domain types, so the generated OpenAPI ([ADR-0019](0019-openapi-generation.md)) documents an
  explicit public contract rather than leaking internal representations.

- **Guest note in, never out.** A line's free-text `note` is accepted and passed as a
  [`GuestNote`](../../crates/pos-proto/src/text.rs), which by construction cannot enter the event log
  ([ADR-0026](0026-port-shapes.md)); the endpoint adds no second path for it.

**Rejected.**

- **Trusting the request's `store_id` without a tenant check** — rejected: it is the cross-tenant hole
  the whole API-key design exists to prevent. The `StoreDirectory` lookup is not optional.
- **Implementing `OrderIn` in the cloud** — rejected. Offline acceptance (contract rule 5) makes the
  store's edge the real implementor; the cloud relays. The endpoint stays a caller.
- **Reusing an existing read scope** (e.g. `ReadEvents`) for submission — rejected: a read grant must
  never authorise a write. `PlaceOrders` is separate.
- **Serving the route and documenting it in OpenAPI before the cloud→store relay exists** — rejected
  as premature. This slice is the intake *library* — the router, the validation, the tenant binding,
  the acceptance mapping — proven against the fakes. Merging it into the served router and registering
  it in `docs/openapi.json` lands with the relay (P11a-2), so the public contract document never
  advertises an endpoint that answers nowhere.

**Consequences.**

- **The shared intake path exists and is tested** — QR ordering ([ADR-0012](0012-qr-ordering-via-cloud.md))
  and the marketplace adapters become callers of it rather than new pipelines, which is the roadmap's
  reason to build it first.
- **Cross-tenant submission is closed by construction**, and the closure is a test, not a convention.
- **Composition waits on one buildable piece** — the cloud→store `OrderIn` relay — not on any external
  decision; A1 (card terminals) and A3 (ShopeeFood channel) block the payment and vendor adapters,
  never this path.
- **Nothing is foreclosed.** New callers reuse the port; the relay stays behind `OrderIn`; the tenant
  lookup stays behind `StoreDirectory`.
