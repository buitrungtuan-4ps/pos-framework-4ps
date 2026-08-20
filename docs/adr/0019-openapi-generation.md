# ADR-0019 — OpenAPI is generated from the code, and drift fails CI

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0016](0016-postgres-access.md) · [ADR-0018](0018-http-websocket-stack.md) · [ADR-0010](0010-naming-standard.md)

**Context.** `pos_cloud` (P7) exposes a public `/v1` API — the surface external integrators build
against. Its OpenAPI document is a contract, and a contract that is **hand-written beside** the code
drifts the first time a field is added and nobody updates the YAML: the document says one thing, the
server does another, and the integrator debugging the difference is the last to find out. The
specification is explicit that the OpenAPI is *generated, never transcribed*.

**Decision.** The OpenAPI document is **generated from the Rust handlers and types** with
[`utoipa`](0007-in-house-vs-dependency.md), and a CI **drift check** regenerates it and fails if it
differs from the committed copy.

- **Types carry their own schema.** The `/v1` request and response types derive `utoipa::ToSchema`
  next to their `serde` derives, so the wire shape and its schema cannot disagree — they are the same
  struct. Field names are the `snake_case` the naming standard ([ADR-0010](0010-naming-standard.md))
  already requires, so the generated schema needs no rename layer.
- **Handlers carry their own paths.** Each `/v1` axum handler ([ADR-0018](0018-http-websocket-stack.md))
  is annotated with its method, path, parameters and responses via `utoipa::path`, and a single
  `OpenApi` derive collects them into the document. Adding a route without annotating it is visible in
  review because the route and its doc live on the same function.
- **The committed document is generated output.** `docs/openapi.json` holds the generated spec, pretty-printed
  so the diff is reviewable line by line; a human never edits it. It is committed so integrators and the
  diff both have a stable artefact to read.
- **A CI gate fails on drift, and it lives in `pos-cloud`.** The gate is a `#[test]` in `pos-cloud`
  (`src/openapi.rs`) that renders the document from the `OpenApi` derive and compares it against the
  committed `docs/openapi.json`; any difference fails `cargo test`, and regeneration is opt-in with
  `POS_UPDATE_SNAPSHOTS=1 cargo test -p pos-cloud openapi` — the **same idiom, environment variable and
  driven-failure discipline** as `pos-proto`'s event-catalogue snapshot ([ADR-0010](0010-naming-standard.md)).
  It is deliberately **not** an `xtask` subcommand: rendering the document requires linking `pos-cloud`'s
  full HTTP and storage tree (axum, `utoipa`, `store-postgres`), and `xtask` is kept dependency-light on
  purpose (it is the tool that must build before anything else does), so the check belongs beside the code
  it checks, not in `xtask`. Changing the API therefore means regenerating the document **in the same pull
  request**, which is the rule that keeps docs and behaviour together.

**Rejected.**

- **Hand-written OpenAPI** — rejected outright; it is the drift this decision exists to prevent.
- **Generating server code *from* an OpenAPI document** (spec-first) — rejected: it puts the source of
  truth in a YAML file a compiler cannot check against the handlers, and the project's discipline is
  that the Rust types are the truth and the documents are generated from them (as with the event
  catalogue snapshot and the state-machine and permission docs).
- **A separate framework that owns routing *and* docs** — rejected as more than needed over the axum
  stack already chosen in [ADR-0018](0018-http-websocket-stack.md); `utoipa` annotates the handlers
  that stack already has.

**Consequences.**

- `utoipa` (and a small serve-time UI such as `utoipa-swagger-ui`, if we expose one) join the cloud
  dependency surface, reviewed by `cargo-deny`.
- The drift check is a `pos-cloud` crate test that renders `docs/openapi.json` and asserts equality, run
  by the ordinary `test` job — the same mechanism as the event-catalogue snapshot, so it needs no new CI
  wiring. Its driven failure (deleting a field and watching the assertion fire) is exercised the same way
  the snapshot gate's is.
- The generated document covers the public `/v1` surface only; internal admin and ingest endpoints are
  not part of the external contract and are documented separately where useful.
