# ADR-0180 — utoipa in pos-cloud-config and pos-ports

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-26 · Extends
[ADR-0177](0177-openapi-is-the-route-map.md) · Relates to
[ADR-0019](0019-openapi-generation.md), [ADR-0021](0021-corrected-port-list.md),
[ADR-0033](0033-config-tree.md)

The owner approved D-T1, D-T2 and D-T3 below on 2026-10-26.

## Context

[ADR-0177](0177-openapi-is-the-route-map.md) gives every route a response schema, taken from the
`pos-proto` wire types behind that crate's off-by-default `openapi` feature. Three groups of answers
cannot get one under the dependencies allowed today:

- **`pos-cloud-config`'s own records** (config versions, settings, the org registry, store groups,
  data migrations). The other cloud area crates (`pos-cloud-api`, `-catalog`, `-devices`, `-floor`,
  `-inventory`, `-people`, `-reports`) already depend on `utoipa`. `pos-cloud-config` does not, so
  its records could only get a schema from a hand-written mirror in `pos-cloud`, held to its type by
  a test.
- **`pos-ports` types on the wire.** The till's `GET /api/print/jobs` answers with the printer
  port's job type, and the config pull carries `ConfigUpdate`. `pos-ports` is a backbone crate: its
  dependencies are on the allow-list `xtask deps-rule` enforces
  (`tools/backbone-allowlist.toml`), and the store binary links it.
- **Config nodes answered as stored JSON.** A node is kept as `serde_json::Value`, so a route that
  returns one has no type to derive a schema from.

## Options considered

1. **Keep mirroring by hand.** No dependency moves, but every mirror is a second description that a
   test must hold to the first, and some (`ConfigUpdate`'s tagged union) are long.
2. **Move the served types into `pos-proto`.** It already has the feature. But a port's type belongs
   with its port (ADR-0021); moving it to make a schema is the tail wagging the dog.
3. **Give each crate `utoipa` the way its layer allows.** A normal dependency in the cloud crate,
   and an optional one, off by default, in the backbone crate, exactly as `pos-proto` has it.

## Decision

Option 3.

- **D-T1.** `utoipa` becomes a normal dependency of `pos-cloud-config`, as for the other cloud area
  crates. Its records derive `ToSchema` directly, which is preferred to a hand-written mirror. A
  mirror stays only where a derive cannot express the type's `Serialize`, with a test holding it.
- **D-T2.** `pos-ports` gets an `openapi` feature, off by default, that enables an optional
  `utoipa` and `pos-proto/openapi`, as `pos-proto` did. The served types derive `ToSchema` under
  `cfg_attr(feature = "openapi", …)`. `utoipa` stays **optional-only** in
  `tools/backbone-allowlist.toml`, now for `pos-ports` as well as `pos-proto`, and `xtask deps-rule`
  fails a default feature that turns it on. The store binary's default build links nothing new.
  `pos-edge`'s own `openapi` feature turns `pos-ports/openapi` on.
- **D-T3.** A config node answered as stored JSON gets a schema by what the route knows:
  - where the route fixes the node's kind, the node's `pos-proto` type;
  - where it does not, and the JSON carries a discriminator, a `oneOf` of the kinds with that
    discriminator;
  - otherwise a free-form JSON object, with a description saying so and naming where the node's
    shape is defined.

Kept from the earlier technical calls:

- Key order in a regenerated document follows declaration order. A pull request that regenerates one
  says "values identical, key order may differ".
- An `Open<E>` token stays `string` in a schema, so a new token never breaks an old client.

## Consequences

- No route, port signature, protocol or `PROTOCOL_VERSION` changes. The documents gain schemas;
  their values are additive.
- `pos-ports` and `pos-proto` are the only backbone crates with an `openapi` feature; `pos-core` has
  none. A third would need its own record.
- Nothing is wired up by this record. The `pos-cloud-config` and `pos-ports` schema slices follow,
  each with its tests, and the allow-list's `utoipa` reason is updated in the `pos-ports` slice.
- A free-form node schema (D-T3's last case) tells the front end nothing about its shape. Each one
  is listed in its route's description, so it can be narrowed when its kind is fixed.
