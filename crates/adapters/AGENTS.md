# `crates/adapters` — local rules

Read the root [`AGENTS.md`](../../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** one crate per external system: a database, a broker, a printer, a vendor.
Each implements one or more ports from `pos-ports`. Each crate's `README.md` says which.

## Local rules

- An adapter implements a port. It never changes one: a port change needs an ADR first.
- Every adapter runs its port's shared suite from `pos-contract-tests`
  ([ADR-0026](../../docs/adr/0026-port-shapes.md)). A suite that needs a live server
  (PostgreSQL, NATS, S3, an OS keyring) sits behind the crate's `integration` feature, so the
  pull-request `test` job builds without it.
- Put the socket behind a transport trait and keep the mapping pure (request bodies, status
  vocabularies, HTTP status to `PortError`), so the suite runs against a stub in the fast gate.
- A new adapter starts from [`templates/adapter-template`](../../templates/adapter-template/README.md),
  joins the workspace `members`, and gets a README and a contract card (`xtask crates`).
- A new third-party dependency needs an ADR first and must pass `cargo deny`.
- Migrations live in `store-postgres/migrations/` and `store-sqlite/migrations/`. A shipped
  migration is never edited, and a migration never drops or renames
  ([ADR-0017](../../docs/adr/0017-migrations.md)).
- A vendor adapter exports a `ProviderDescriptor` for the catalogue
  ([ADR-0153](../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)). The core never
  names a vendor (`xtask vendor-neutral-core`).

## Most often touched

`store-postgres/src/store.rs` and `src/<area>.rs` · `store-postgres/tests/integration.rs` ·
`store-sqlite/src/writer.rs` and `src/store.rs`

## Gates

`cargo test -p <adapter>` · `cargo test -p <adapter> --features integration` with its server ·
the `postgres` job · the `contract` job in `main.yml` · `xtask migrations` · `xtask crates` · `deny` ·
`changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../../changes/README.md)).

## Read first

[`docs/guides/write-an-adapter.md`](../../docs/guides/write-an-adapter.md) ·
[`docs/architecture.md`](../../docs/architecture.md) §5 ·
[ADR-0021](../../docs/adr/0021-corrected-port-list.md).
