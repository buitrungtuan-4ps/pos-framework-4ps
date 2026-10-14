# `crates/pos-proto` — local rules

Read the root [`AGENTS.md`](../../AGENTS.md) first. This file adds to it and never overrides it.

**What lives here:** everything that crosses a boundary. The event catalogue and envelope, value
types (money, quantities, identifiers, time), wire enums, the error envelope, the settings
register, and `PROTOCOL_VERSION`. See [`README.md`](README.md).

## Local rules

- **An ADR is merged before any change here** (root `AGENTS.md` §2, §7).
- Changes are additive. A published event type, payload field, setting or wire enum value is never
  renamed or removed; deprecate it. A new optional field is
  `#[serde(default, skip_serializing_if = …)]`, so an old event's bytes do not change, and a test
  pins those bytes.
- A breaking change needs `PROTOCOL_VERSION` raised, which the additive rule is there to avoid
  ([ADR-0024](../../docs/adr/0024-protocol-version-negotiation.md)).
- No I/O, no runtime, no procedural macros, and nothing outside `tools/backbone-allowlist.toml`.
- Names follow [`docs/naming-and-api.md`](../../docs/naming-and-api.md): `domain.resource.action`
  in the past tense for events, `_time` for instants, `currency_code` and `amount_minor` for money.
- After a change, `just snapshot` regenerates `docs/snapshots/events.txt`, `settings.txt` and
  `docs/configuration.md`. Never edit them by hand (root `AGENTS.md` §2).

## Most often touched

`src/settings.rs` (the register) · `src/events.rs` · `src/lib.rs` · `src/shift.rs` · `src/fees.rs` ·
`src/devices.rs`

## Gates

`cargo test -p pos-proto` (snapshot and byte-for-byte tests) · `xtask snapshot` (no removals) ·
`xtask deps-rule` · the clippy backbone pass · `xtask vendor-neutral-core` · owner review
(`@maintainers-architecture`) · `changes/<topic>.md` for a user-visible change (`docs-gate`, [`changes/README.md`](../../changes/README.md)).

## Read first

[ADR-0010](../../docs/adr/0010-naming-standard.md) ·
[ADR-0160](../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md) ·
the [repository map](../../docs/repository-map.md) sections on events and settings.
