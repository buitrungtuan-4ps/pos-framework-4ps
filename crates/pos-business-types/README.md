# `pos-business-types`

**Layer** `app` · **Owners** @maintainers-architecture

## What it is

The business-type templates shipped in [`business-types/`](../../business-types/), read and checked
against the settings register and the country packs
([ADR-0200](../../docs/adr/0200-six-business-type-templates-ship-with-the-framework.md)).

## Owns

- Which templates a release ships: one `business-types/<id>/template.json` per `BusinessType`,
  compiled in (`templates`, `template`, `directory`).
- The checks on a template's recommended settings: a register key, a value the setting takes, no
  key twice, no `package.*` key, and (in its tests) no key any shipped country pack sets.

## Depends on / used by

- **Depends on** (workspace crates): `pos-core`, `pos-proto`; in tests, `pos-country`, `pos-country-in`, `pos-country-jp`, `pos-country-vn`, `pos-country-zz`.
- **Used by** (workspace crates): no workspace crate.

## Entry points

- [`src/lib.rs`](src/lib.rs)

## Rules

- The root [`AGENTS.md`](../../AGENTS.md) applies, and so does any `AGENTS.md` in a directory above
  this one.

## How to test

`cargo test -p pos-business-types`
