# Add a country module

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-21

A country module holds everything specific to one country: its tax-invoice rules (`Fiscalization`),
its locale pack (currency, timezone, date/number formats, receipt templates, channel-keyed tax rates),
and any local vendors. The core never changes when you add one, and a fork that serves two of five
countries compiles only those two ([ADR-0027](../adr/0027-country-modules.md)).

The reference module is [`countries/zz`](../../countries/zz) — copy it.

## Adding a country is exactly three edits

`cc` is the lowercase ISO country code (e.g. `vn`, `jp`). The crate **must** be named `pos-country-<cc>`.

1. **The module itself** — `countries/<cc>/` with `Cargo.toml` (`name = "pos-country-<cc>"`), `src/lib.rs`,
   and `tests/`. Copy `countries/zz` and rename.
2. **The workspace** — add `"countries/<cc>"` to `members` in the root `Cargo.toml`. Without this the
   directory is never compiled, linted, or tested — it *looks* done and isn't.
3. **Each binary that should carry it** — add a `country-<cc>` feature in `crates/pos-edge/Cargo.toml`
   (and `pos-cloud` if it needs country logic):
   ```toml
   [features]
   country-vn = ["dep:pos-country-vn"]
   [dependencies]
   pos-country-vn = { path = "../../countries/vn", optional = true }
   ```
   A country wired into the workspace but into no binary's features compiles and tests green yet can
   never be selected — which is why edit 3 exists.

All three edits are checked: `cargo xtask countries` (run by `just links` and `just preflight`) fails
if a `countries/` directory is not a member, is misnamed, or is selectable from no binary. So a
half-finished country fails CI rather than looking finished.

## Fill in the country's obligations

- **Locale pack** — currency, timezone, formats, receipt templates, and the **channel-keyed tax
  rates** (`tax_class` × sales channel; the schema carries this from day one even where v1 uses one
  rate). Types are in `pos-proto`'s locale module.
- **`Fiscalization`** — if the country has electronic tax invoices, implement the port
  (allocate-range / issue / look-up / reconcile). Its contract suite in `pos-contract-tests` is the
  spec. Invoices key on the **calendar** date, never the business date.

## Select it in a build

```bash
cargo run -p pos-edge --no-default-features --features country-vn
```

A fork edits one line in `default = [...]` to bake its country in. Deleting a country you do not ship
is deleting nothing — you simply never enable its feature.
