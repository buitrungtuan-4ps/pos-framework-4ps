# Add a country module

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-20

A country module holds everything specific to one country: its tax-invoice rules (`Fiscalization`),
its locale pack (currency, timezone, date/number formats, receipt templates, channel-keyed tax rates),
and any local vendors. The core never changes when you add one, and a fork that serves two of five
countries compiles only those two ([ADR-0027](../adr/0027-country-modules.md)).

Generate it ([ADR-0176](../adr/0176-the-adapter-and-country-standard.md)). Every value the
country's law sets is a required flag, and the command refuses to run without one:

```bash
cargo xtask new country <cc> --display-name <name> --currency <ISO 4217> --exponent <0-4> \
  --prices-include-tax <true|false> --cash-rounding <none|minor units> \
  --tax-rate food=<basis points> --tax-rate alcohol=<bp> --tax-rate exempt=<bp> \
  [--tax-rate food@takeaway=<bp>] --retention-days <n> --language <tag>
```

It writes `countries/<cc>` and does the three edits below for you, in `pos-edge` and `pos-cloud`
alike, and adds the country to no default feature. The module's `Fiscalization` is the shared
offline allocator, so its contract suite passes from the first build. It prints the neighbouring
README lines to paste, which `cargo xtask crates` also shows. Then fill in what it left neutral,
under [Fill in the country's obligations](#fill-in-the-countrys-obligations): the number format, the
tax-code rule, the cash denominations and the invoice number's shape.

The reference module is [`countries/zz`](../../countries/zz). Three real ones are
written against the same shape and are the better model for a market you actually intend to open:
[`countries/vn`](../../countries/vn), [`countries/jp`](../../countries/jp) and
[`countries/in`](../../countries/in) ([ADR-0105](../adr/0105-a-country-pack-is-values.md)). Read the
one closest to yours: Japan if your tax varies by channel, India if your invoice must print the
rate's components, Vietnam if neither.

## What the three edits are

`cargo xtask new country` makes these. They are listed here for review, and for adding a country
by hand.

`cc` is the lowercase ISO country code (e.g. `vn`, `jp`). The crate **must** be named `pos-country-<cc>`.

1. **The module itself** — `countries/<cc>/` with `Cargo.toml` (`name = "pos-country-<cc>"`), `src/lib.rs`,
   and `tests/`.
2. **The workspace** — add `"countries/<cc>"` to `members` in the root `Cargo.toml`. Without this the
   directory is never compiled, linted, or tested — it *looks* done and isn't.
3. **Each binary that should carry it** — add a `country-<cc>` feature in `crates/pos-edge/Cargo.toml`
   and `crates/pos-cloud/Cargo.toml`, and an arm in each binary's `src/countries.rs`:
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
  Also fill in the three **till** facts ADR-0105 added, because each is wrong by default in some
  market: `prices_include_tax` (Japan and India quote inclusive, Vietnam exclusive),
  `cash_rounding_increment` (India ₹1, Vietnam 1,000 ₫, Japan none), and `cash_denominations` (the
  notes a guest hands over, in minor units — empty means "the exact amount only").
  `default_language` is also what a new store's receipts print in
  ([ADR-0160](../adr/0160-everything-a-store-runs-differently-is-published-configuration.md)), where
  [`paper_labels.rs`](../../crates/pos-edge-printing/src/paper_labels.rs) has a table for it.
- **`Fiscalization`** — if the country has electronic tax invoices, implement the port
  (allocate-range / issue / look-up / reconcile). Its contract suite in `pos-contract-tests` is the
  spec. Invoices key on the **calendar** date, never the business date.

  Most of that port is the same everywhere, so start from
  [`pos_country::offline::OfflineFiscalization`](../../crates/pos-country/src/offline.rs): it holds
  pre-allocated ranges, never-reuse, one-number-per-bill and refusal-on-exhaustion, and takes your
  country's number format. A provider with a real authority **wraps** it — keeping the offline path,
  which is the one that matters when the line drops — rather than replacing it.

## Select it in a build

```bash
cargo run -p pos-edge --no-default-features --features country-vn
```

A fork edits one line in `default = [...]` to bake its country in. Deleting a country you do not ship
is deleting nothing — you simply never enable its feature.
