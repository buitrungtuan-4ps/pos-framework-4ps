# `crates/pos-core` — local rules

Read the root [`AGENTS.md`](../../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** the domain. State machines, money arithmetic, the permission catalogue,
capability flags, pricing, fees and inventory rules, as `decide(state, command, ctx) ->
Result<Decision, DomainError>`. See [`README.md`](README.md).

## Local rules

- No I/O and nothing async. The crate does not depend on `pos-ports`, and that is deliberate
  ([ADR-0013](../../docs/adr/0013-async-strategy.md)). Do not add that dependency.
- Time and identifiers arrive as values in the context. Never read a clock or a random source.
- A new rule extends the domain with a new case or a new decision. It never branches on a vendor,
  a country, or a store's name ([`design-principles.md`](../../docs/design-principles.md)).
- A permission exists only as an entry in `permissions!`, with its default roles. Adding one
  regenerates `docs/permissions.md` and `docs/snapshots/permissions.txt`, and a PIN-flagged one
  needs a backfill migration ([repository map](../../docs/repository-map.md#permission)).
- Changing how money, tax or receipt numbering works needs an ADR first.

## Most often touched

`src/decision.rs` (the `decide_*` functions) · `src/billing.rs` · `src/permission.rs` ·
`src/machines.rs` · `src/floor.rs`

## Gates

`cargo test -p pos-core` · `xtask deps-rule` (the allow-list in `tools/backbone-allowlist.toml`) ·
the clippy backbone pass (no `unwrap`, `expect`, `panic!`) · `xtask vendor-neutral-core` ·
`xtask snapshot` · owner review (`@maintainers-domain`) ·
`changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../../changes/README.md)).

## Read first

[`docs/pos-spec.md`](../../docs/pos-spec.md) for the rule in prose ·
[`docs/state-machines.md`](../../docs/state-machines.md) ·
[ADR-0028](../../docs/adr/0028-settlement-and-payment-invariant.md) and
[ADR-0158](../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md).
