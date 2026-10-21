---
kind: added
---
- **`pos-proto` gains an off-by-default `openapi` feature that gives its wire types OpenAPI schemas**
  (ADR-0177 decision 3). With it, every type that derives serde's traits also derives
  `utoipa::ToSchema`, each `wire_enum!` enum, including those declared in other crates, publishes
  its tokens, and the identifiers, instants, dates, currency and country codes and event-type
  names, whose `Serialize` is hand-written, get hand-written schemas that a test holds to what they
  serialize. `pos-cloud` enables it, and `pos-edge` only through its own `openapi` feature, so the
  store binary's default build links no `utoipa`. `tools/backbone-allowlist.toml` admits `utoipa`
  as optional-only, and `cargo xtask deps-rule` now fails if an optional-only crate is declared
  unconditionally or turned on by a default feature. No document changes yet: the routes start
  naming these schemas in later slices.
