# ADR-0039 — Config reaches the store by authenticated pull on a store-facing `/sync` surface

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0026](0026-port-shapes.md) · [ADR-0031](0031-cloud-adapter-transports.md) · [ADR-0033](0033-config-tree.md) · [ADR-0037](0037-api-keys.md)

**Context.** [ADR-0033](0033-config-tree.md) built the config tree and left one piece for later: the
cloud composes, validates, versions, and persists a store's configuration, and
`ConfigTree::update_for(held)` already computes exactly what a store should receive — nothing
(`UpToDate`), a collapsed RFC-7386 delta, or a full snapshot past *K* versions behind — but **the
cloud does not yet hand that output to the store over the wire.** The store's apply side already
exists behind the `ConfigStore` port ([ADR-0026](0026-port-shapes.md)). What is undecided is the
*channel*: how the update crosses from cloud to store, and how the store authenticates to ask for it.

The constraint that settles it: **there is no cloud→store push channel, by design.** The store→cloud
link is NATS JetStream, and [ADR-0031](0031-cloud-adapter-transports.md) fixed it as **outbound
only** — the store publishes its event log to the cloud and the cloud never publishes back over it.
Adding an inbound push channel (a broker subscription the store listens on, a long-poll the cloud
holds open) would be a new load-bearing piece of infrastructure the architecture has deliberately
avoided.

**Decision.**

- **The store pulls; the cloud does not push.** Configuration delivery is store-initiated: the store
  asks "given the version I hold, what should I apply?" and the cloud answers with `update_for`'s
  `SyncOutcome`. This is the only shape consistent with the outbound-only link, and it fits the edge
  as already built — the edge holds a current version behind `ConfigStore` and hot-reloads a new one
  with last-known-good ([ADR-0033](0033-config-tree.md)), so a poll-and-apply loop is a client of
  machinery that already exists. "Delta *publishing*" is realized as delta *serving*: the cloud does
  the diff/snapshot decision, the store does the fetch.

- **A dedicated store-facing surface, `/sync`, separate from the four existing route families.** The
  cloud already separates its HTTP surface by audience — `/health` (liveness), `/internal` (the
  reconciliation re-push, private network only), `/v1` (the **public** integrator API, OpenAPI-
  documented), `/admin` (the interactive super-admin surface). Store configuration delivery is a
  fifth audience: **a first-party store fetching its own operational state**, not an integrator
  reading business data. It gets its own prefix, `GET /sync/stores/{store_id}/config`, and is
  **absent from the public OpenAPI document** for the same reason `/admin` and `/internal` are — it
  is not part of the contract external integrators build against. `?held_version=<ULID>` names the
  version the store holds (omitted means "I have nothing"); the response is `{"status":"up_to_date"}`
  or `{"status":"update","update":{…}}` carrying the snapshot or delta the store applies.

- **It is authenticated by the API-key bearer with a new `read_config` scope, tenant-isolated.**
  The store→cloud credential that already exists is the scoped per-tenant API key
  ([ADR-0037](0037-api-keys.md)); the `/sync` route reuses it rather than inventing a second
  mechanism. A new deny-by-default scope, `read_config`, authorizes config pull and nothing else, and
  the route answers **only for the key's own tenant** — the tenant comes from the verified grant, not
  the path, so a store can fetch a `store_id` only within its own tenant (a `store_id` outside it
  simply has no tree and reads `404`), the same isolation boundary the rollups read draws.

**Rejected.**

- **Push over a cloud→store channel** (a second NATS stream, an MQTT topic, a held-open long-poll) —
  rejected: it reverses the outbound-only link [ADR-0031](0031-cloud-adapter-transports.md) fixed and
  adds inbound infrastructure the store must expose or hold open. A store that has been offline still
  needs to *catch up on reconnect*, which is a pull regardless; making the steady state a pull too
  keeps one path, not two.
- **Serving config under `/v1`** (the public API) — rejected: config delivery to first-party stores
  is operational, not an integrator API, and mixing it into the public contract would put it in the
  generated OpenAPI and couple its wire types to `utoipa::ToSchema`. A separate `/sync` surface keeps
  the public contract about integrator data and the store-facing surface about store operation.
- **A device-scoped credential** (a per-store secret from an activation code) — the *right* long-term
  credential, but it belongs to P9 (activation codes → device credentials in the TPM/keyring). For
  P7 the tenant-scoped `read_config` key is the pragmatic credential; narrowing it to a single store
  is a later tightening behind the same route.

**Consequences.**

- **No new dependency and no new collaborator.** The route reuses the `ApiKeyStore` bearer and the
  `ConfigTreeStore` already in `CloudApp`; it adds one `Scope` variant and one handler. The response
  is a small serde DTO wrapping the port's already-`Serialize` `ConfigUpdate`.
- **The delivery decision stays in the pure engine.** `update_for` — the delta/snapshot/up-to-date
  choice and the *K* threshold — is `pos-core`-adjacent pure logic already unit-tested without a
  database; the route is a thin authenticated wrapper, tested over the fakes (up-to-date, delta,
  snapshot, unknown store `404`, missing scope `403`, no key `401`).
- **The edge's polling client is the complementary piece, and is not in this slice.** This lands the
  cloud's *serving* side — the missing half [ADR-0033](0033-config-tree.md) named. The `pos_edge`
  loop that calls `/sync` on an interval and applies the result through `ConfigStore` is store-side
  fleet wiring (P9); the contract between them is this route's request/response shape.
- **Nothing is foreclosed.** Delivery is a route behind a scope, so tightening the credential to a
  device (P9) or, if the link model ever changes, adding a push notification that merely *triggers*
  the same pull, are both changes to how the pull is invoked, not to the config engine.
