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
  not part of the external contract and are documented separately where useful. *(Amended 2026-09-03 —
  see below: `/admin` now has a generated document of its own.)*

---

## Amendment 2026-09-03 — the console surface gets its own document (roadmap v3 B5)

That last consequence — "documented separately where useful" — deferred a decision the console needed
answering: `/admin` had grown to **137 routes** against `docs/openapi.json`'s **2**, and a fork writing
its own console had nothing to write against. This amendment settles the three questions that had to
be answered before any of it could be generated.

**Two documents, not one.** `/v1/openapi.json` stays the *integrator* contract — what an outside
system calls with a scoped API key. `/admin/openapi.json` (committed as `docs/openapi-admin.json`) is
the *console* contract, with its own title, audience, and security scheme (the host-only session
cookie rather than a bearer key). Folding a hundred and thirty-seven console routes into the
integrator document would bury the three routes an integrator came for; the audiences share no
overlap, so neither does the paperwork.

**Requests and outcomes are documented; success bodies are not.** Each documented route carries its
path, method, path and query parameters, `If-Match` where a write is conditional, every status code it
can answer, and the AIP-193 error envelope those failures carry. A success body is described in prose.

That stop is forced, not lazy. An `/admin` handler returns a **`pos-proto` wire type directly** —
[ADR-0079](0079-inventory-and-suppliers.md) and its siblings made the authored record *be* the wire
type so there is no second cloud-side shape to keep in sync. Generating schemas from those types means
`utoipa::ToSchema` on them, which means `utoipa` inside `pos-proto`: a backbone crate under the
forbid-pass, governed by `tools/backbone-allowlist.toml`, which the `deps-rule` gate requires an ADR to
change. The alternative — hand-mirroring forty wire types as cloud-local schema DTOs — reintroduces
exactly the duplication ADR-0079 removed. Both are real options and both are larger than the value of a
response schema in a document whose reader already has the Rust types; if a consumer ever needs the
schemas, the backbone dependency is the honest way and it gets its own ADR.

The error envelope is the **one** exception, mirrored as a local `ToSchema` type: it is one type rather
than forty, and it is the half of the contract a client must branch on. A test compares the mirror's
fields against a real serialized `pos_proto::error::ErrorResponse`, so it cannot drift silently.

**Coverage is partial, and the gap is in the repository.** The document describes 7 paths today; the
other 130 are named in an `UNDOCUMENTED` constant. That list is not a to-do comment — four tests hold
it honest:

- the committed document matches what the code generates (the same gate `/v1` has);
- every registered `/admin` route is either documented or listed, so **a route added tomorrow lands in
  neither and fails the build**;
- no documented path is absent from the router, so a hand-edited `path =` cannot promise a 404;
- nothing in the list is already documented or already deleted, so the list cannot rot in either
  direction.

The registered set is recovered by reading the router's own source for `.route("…")` literals, because
axum's `Router` does not expose its routes. When the list reaches empty it is deleted and the second
test becomes "every route is documented", with nothing further to do.

**One correction to the record.** The claim that reaching this point needed a new dependency was wrong,
and was stated to the owner before being checked: `utoipa` has been a `pos-cloud` dependency since this
ADR, and the drift-gate mechanism this amendment reuses was already built here. What is genuinely
gated on an ADR is only the `pos-proto` schema derive above — the fidelity question, not the machinery.
