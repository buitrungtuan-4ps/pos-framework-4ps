# ADR-0024 — `PROTOCOL_VERSION` negotiation

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

**Context.** [`naming-and-api.md`](../naming-and-api.md) §11 fixes the rules: the product
version and `PROTOCOL_VERSION` are independent axes, the cloud must understand at least the
two most recent protocol versions, protocol changes are additive, and a breaking change runs
both versions in parallel for at least two releases. What it does not say is *where the
version rides on the wire, how the two sides agree, or what happens when they cannot*. Edges
update in rings and may be offline for days, so several edge versions are always connected to
one cloud at once — this is the normal state, not an error.

**Options considered.**

1. Stamp the version on every event envelope and infer compatibility per message. Rejected:
   it discovers the mismatch once per event forever, and the envelope already carries
   `schema_version`, which is a *different* axis — payload evolution, not the language.
   Conflating them would mean one field with two meanings.
2. Version the NATS subject (`pos.v3.store.…`). Rejected: it makes every subject change a
   protocol change, and it hides the negotiation in naming where nothing can report on it.
3. **Negotiate once per connection, then keep the agreed version for the session.** Chosen.

**Decision.**

*The handshake.* Every edge→cloud connection opens with a `hello` frame carrying
`protocol_version_min`, `protocol_version_max`, `product_version`, `store_id` and the
current lease token. The cloud answers with the single `protocol_version` it will speak for
the session — the highest version both sides support — or a typed refusal. Both sides then
hold that number for the life of the connection; nothing renegotiates mid-session.

*The floor.* The cloud accepts `[CLOUD_MAX − 1, CLOUD_MAX]` at minimum. A CI test asserts
it, so dropping support for a version that is still in the fleet fails the build rather than
the rollout.

*`schema_version` stays separate.* `PROTOCOL_VERSION` is the language the two tiers speak;
`schema_version` on the envelope is the shape of one event's payload. The public API's
optional `pos-api-version` header is a third, unrelated thing — a minor-version pin for
external callers. Three axes, three names, never mixed.

*Refusal behaviour, and this is the part that matters.* An edge the cloud cannot speak to
receives `FAILED_PRECONDITION` naming the minimum supported version. It then:

- **keeps selling** — a protocol mismatch degrades to "not syncing", never to "not selling";
- retains everything in the outbox, since nothing is lost by waiting;
- backs off with exponential delay and jitter rather than retrying in a tight loop;
- raises the condition on the fleet dashboard and in the store's own status bar, because a
  store that has silently stopped syncing is the failure worth surfacing loudest;
- resumes and drains automatically once an OTA brings it forward.

An edge *newer* than the cloud should not occur, because the cloud is upgraded before edge
rings roll. If it does, the edge downgrades to the cloud's version when that is inside its
supported range, and otherwise takes the same refusal path.

**Consequences.**

- One negotiation per connection instead of a compatibility check per event.
- The "cloud supports the last two versions" rule becomes a test rather than a promise.
- Because refusal is non-fatal at the store, a botched protocol change costs synchronisation
  latency and an alert — not revenue. This is the same property [ADR-0001](0001-offline-first-store-autonomy.md)
  buys everywhere else, applied to our own mistakes.
- The handshake is a natural place to carry the lease token, so single-active enforcement
  ([ADR-0003](0003-cattle-not-pets.md)) and version negotiation share one round trip.
- Adding a field to `hello` is additive and needs no version bump; removing one does.
