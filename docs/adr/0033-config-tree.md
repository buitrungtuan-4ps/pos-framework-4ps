# ADR-0033 — The four-level config tree: deep-merge layers, RFC 7386 deltas, K-bounded snapshots

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0004](0004-cloud-owned-configuration.md) · [ADR-0010](0010-naming-standard.md) · [ADR-0026](0026-port-shapes.md) · `docs/pos-spec.md` §10 · `docs/roadmap.md` D10

**Context.** Configuration is cloud-owned ([ADR-0004](0004-cloud-owned-configuration.md)) and the
`ConfigStore` port ([ADR-0026](0026-port-shapes.md)) already fixes the *store's* side: hold a current
version, apply a snapshot or a delta, keep last-known-good. It deliberately left three things for
this slice, which `docs/roadmap.md` D10 lists as open: the **four-level tree** (Tenant → Brand →
Store → Device), the **delta and snapshot format**, and the value of ***K*** in "more than *K*
versions behind ⇒ full snapshot." A store must be able to sell offline, so a bad or malformed version
must never reach it, and a store that has been offline for a while must be able to catch up cheaply.

**Decision.**

- **Four levels, deep-merged, most-specific wins.** A store's effective document is the deep merge
  of four authored layers in order: Tenant, then Brand, then Store, then Device. Objects merge
  recursively; a scalar, array, or `null` at a more-specific level replaces what a broader level had.
  So a brand sets a default and a store overrides one key of it without restating the rest. Keys are
  the `snake_case`, `_enabled`-suffixed names the naming standard already fixes
  ([ADR-0010](0010-naming-standard.md), D8).

- **The delta format is RFC 7386 JSON Merge Patch.** A delta is a JSON document where a present key
  overrides, a nested object recurses, and **`null` deletes** the key — the one edit a plain
  overwrite cannot express. It is chosen over RFC 6902 (JSON Patch) because a merge patch is
  *order-independent and idempotent*: there is no array of positional operations to apply in sequence
  and get wrong, which matters for a document a store applies unattended. `diff(from, to)` produces
  the minimal patch and `apply(from, diff) == to` round-trips — the property the engine's tests
  assert over many pairs. A delta from a store *N* versions back is **one collapsed patch** from the
  held effective document to the current one, not a chain of *N*, so applying it is a single step.

- **A version is validated in the cloud before it is published, and rejection keeps last-good.**
  Composing the layers yields a candidate effective document; it is validated, and only if it
  validates does it become a new version. Validation runs `pos-core`'s **§10 inter-flag capability
  rules** (`capability::conflicts`) — the *same* rules the domain enforces, so the cloud cannot bless
  a flag combination the edge would reject — over the flags read from the document (each defaulting
  to its declared default). A rejected publish changes nothing, so the last good version stays
  current; a bad edit degrades an admin to "your change was refused," never a store to "not selling."

- **Snapshot vs delta is keyed on *K*, default 20.** The cloud retains recent versions to diff
  against. A store reports the version it holds; if that version is still retained and within *K* of
  current, it gets a delta; if it is more than *K* behind, or the cloud no longer holds it, it gets a
  full snapshot. *K* is the history depth the cloud keeps per store: 20 is comfortably more than a
  store misses in normal operation, and small enough that the retained history and the largest
  possible diff stay bounded. It is configurable, so an operator can trade memory for fewer
  resyncs. A store exactly at current is told it is up to date and sent nothing.

**Rejected.**

- **RFC 6902 JSON Patch** for deltas — rejected: its positional, ordered operations (`add`/`remove`/
  `replace` at JSON-pointer paths) are more expressive but fragile to apply out of context and
  awkward to generate minimally; merge patch matches how configuration is actually shaped (a tree of
  overrides) and cannot be applied half-way.
- **A per-key typed config schema** validated field-by-field — rejected here: the port deliberately
  carries configuration as opaque JSON so a store on an older build keeps unknown keys
  ([ADR-0026](0026-port-shapes.md)), and a rigid schema would break that forward-compatibility. Flag
  *coherence* is validated (the §10 rules); individual key *types* are the owning feature's concern.
- **Sending the delta chain** (one patch per intervening version) — rejected: a single collapsed
  merge patch reaches the same document in one apply, and the chain only adds failure points.
- **No *K* / always deltas** — rejected: it forces the cloud to retain unbounded history to diff
  against, and a store years behind would receive an enormous patch; the snapshot path bounds both.

**Consequences.**

- `pos-cloud` gains a dependency on `pos-core` (for the capability catalogue and §10 rules). `pos-core`
  is pure — the dependency-rule test keeps it free of I/O — so a binary composing it for validation is
  sound, and reusing it is what stops the cloud and edge drifting on which flag combinations are legal.
- D10's three open questions are closed: the tree shape, the delta/snapshot format (RFC 7386), and
  *K* (20, configurable).
- The engine is pure and I/O-free: composition, the merge patch, validation, and the snapshot/delta
  decision are all unit-tested with no database, including the round-trip and last-good properties.
- **Landed since:** persistence. A `ConfigTreeStore` seam and the `store-postgres` `config_trees`
  table (migration `0004`) persist a store's whole tree — the four authored layers and the published
  history — as one `jsonb` document per `(tenant, store)`, keyed and RLS-isolated by tenant exactly as
  the rollup read model. `ConfigTree::state` / `ConfigTree::from_state` export and rehydrate that
  state (the validator is behaviour, supplied fresh on load; the history is trusted as
  already-validated and not re-run), so a store's tree — and its last good version — survives a
  restart. Round-trip is unit-tested on the engine and against a real database in the adapter's
  integration suite.
- **Landed since:** the admin authoring routes. `PUT /admin/stores/{store_id}/config/{level}` (behind
  the super-admin session guard) loads the store's tree, replaces the named level's document, and
  publishes — composing, validating, and, only if valid, appending a version that is then persisted;
  an incoherent version is a `422` carrying the violations and changes nothing. `GET
  /admin/stores/{store_id}/config` returns the current effective document, or `404` if the store has
  none yet. The tenant is named on the query string (the super-admin is global), and the version id
  is a ULID minted at the edge.
- **Landed since:** the delivery path that hands a `ConfigUpdate` to a store
  ([ADR-0039](0039-config-delivery.md)). Because the store→cloud link is outbound-only
  ([ADR-0031](0031-cloud-adapter-transports.md)), delivery is a store-initiated **pull**:
  `GET /sync/stores/{store_id}/config?held_version=…` runs `update_for` and returns
  `{"status":"up_to_date"}` or the snapshot/delta to apply, authenticated by an API key with a new
  `read_config` scope and answering only for the key's tenant. It is a store-facing surface, absent
  from the public OpenAPI. The `pos_edge` loop that polls it and applies through `ConfigStore` is
  store-side fleet wiring (P9).
- **Deliberately not here yet:** a shared Tenant/Brand layer that fans out to every store under it is
  a future modeling step; today each store's tree holds its own four layers.
