# ADR-0105 — A country pack is a list of values, and none of them are in the framework

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Extends [ADR-0027](0027-country-modules.md) · Builds on [ADR-0104](0104-multi-component-and-inclusive-tax.md)
· Relates to [ADR-0074](0074-localization-and-tax.md), [`pos-spec.md`](../pos-spec.md) §5,
[`roadmap-v3.md`](../roadmap-v3.md) B·W4, B4.5

## The problem

[ADR-0027](0027-country-modules.md) says a country is **supplied**: a directory of constants, a
`Fiscalization` implementation, and three lines of wiring. [ADR-0104](0104-multi-component-and-inclusive-tax.md)
made that true of tax. It is not yet true of the till.

Three facts a market cannot trade without are still held in **code**, and a country module has
nowhere to state any of them.

### 1. Cash rounding exists and no country can set it

`BillInput::cash_rounding_increment` has been there since P3, and the only production caller —
`pos_edge::app` — passes `None`. Not as a default: as a literal.

The three markets disagree, and each for a physical reason:

- **India** rounds the invoice to the nearest rupee. Section 170 of the CGST Act says so, and no
  sub-rupee coin circulates to settle the difference.
- **Vietnam** rounds to the nearest 1,000 đồng, because that is the smallest note anyone carries.
- **Japan** does not round at all, because the 1-yen coin does.

A framework whose answer to all three is `None` is correct in exactly one of them.

### 2. Denominations are a hardcoded table in the front end

`ui/src/lib/money.ts` holds `QUICK_CASH`, a record keyed by currency with three rows in it. It is
honest about its limits — a currency with no row gets only the exact-amount key — but honest is not
the same as supplied. A fork opening in a currency nobody typed into that file gets a till with one
button, and the fix is a front-end edit and a release.

Which notes a guest can hand over is a fact about a **country's cash**, not about a screen.

### 3. A country cannot state its own quoting posture

ADR-0104 put `prices_include_tax` on the store's `locale` node, so a store can be *told* it quotes
tax-inclusive. Nothing lets Japan *say* that every Japanese store does.

That is the difference between a value with a default and a value someone must remember. Whoever
provisions the fortieth Japanese store has to tick the box; forgetting it charges every guest 10 %
more than the menu says, on every bill, until an accountant notices.

## The decision

**A `LocalePack` carries them, because a `LocalePack` is what a country module already returns and
what the configuration tree already publishes.** Three fields, each `#[serde(default)]`, each
overridable per store:

```rust
LocalePack {
    // …country_code, currency_code, tax_rate_table, number_format, default_language,
    //   default_retention_days as before…

    /// Whether menu prices already contain their tax (ADR-0104).
    prices_include_tax: bool,          // JP true, IN true, VN false

    /// What the grand total is rounded to, in minor units, or None for no rounding.
    cash_rounding_increment: Option<i64>,  // VN 1_000, IN 100 (₹1), JP None

    /// The notes a guest hands over, ascending, in minor units.
    cash_denominations: Vec<i64>,      // VN 10k…500k, JP 1k/5k/10k, IN ₹10…₹500
}
```

And two constants that were missing for no reason other than nobody having needed them:
`CountryCode::IN` and `CurrencyCode::INR`.

### Why on the pack and not somewhere new

The country/configuration boundary is already drawn, and these sit on the same side of it as
everything else in the pack: **the country states the fact, the store overrides it.** A store in a
Vietnamese airport that rounds to 5,000 đồng publishes that on its `locale` node and the pack's
1,000 is simply not used — the same relationship `tax_rate_table` has had since ADR-0027.

Putting them anywhere else would create a second place a country is described, and a second place is
a place that will eventually disagree with the first.

### What each field is, precisely

**`cash_rounding_increment` is about cash, not about tax.** Tax rounding is
`BillInput::rounding_mode` and happens per class; this rounds the grand total once, and materialises
as `BillTotals::rounding_adjustment`, an explicit line on the receipt. The reconciliation identity
does not change: `subtotal − discount − comps + service_charge + tax + rounding = total_due`.

**`cash_denominations` is notes, not coins, and it is a till affordance rather than a legal fact.**
Getting it wrong puts an unhelpful button on a screen; getting it empty puts no button there. So an
empty list is a legitimate answer — it means "offer the exact amount only" — and the front end keeps
that behaviour when a store has published no locale.

**`prices_include_tax` on the pack is a default, and the `locale` node still wins.** A country
supplies the posture its law and habit fix; a store that genuinely quotes the other way says so.
Neither is a code change.

## The three packs this exists for

`countries/vn`, `countries/jp` and `countries/in` are written against it, and each is a directory of
constants plus a `Fiscalization` implementation:

| | Vietnam | Japan | India |
|---|---|---|---|
| Currency | VND | JPY | INR |
| Prices quoted | exclusive | inclusive (税込) | inclusive (MRP) |
| Cash rounding | 1,000 ₫ | none | ₹1 |
| Standard rate | 10 % (8 % relief) | 10 %, 8 % takeaway food | 5 %, split CGST 2.5 % + SGST 2.5 % |
| Alcohol | 10 % | 10 %, never reduced | **no row** — state excise and state VAT |
| Tax code | 10 digits, or 13 with a branch | `T` + 13 digits | 15-character GSTIN |
| Grouping | 3 | 3 | **2-2-3** (lakh) |

India's grouping is the one row that is not a constant swap: `NumberFormat::digits_per_group` is a
single number and 12,34,567 is not one. This ADR does **not** widen that field — it records that
India renders as 1,234,567 today, which is wrong-looking rather than wrong, and that fixing it means
making the field a group *pattern*. That is a separate, additive change with its own visual
consequences, and bundling it here would hide it.

## What this deliberately does not do

- **It does not put a registration number in a country pack.** Japan's qualified-invoice number and
  India's GSTIN are per-**store** values a legal process issues, so they belong in the configuration
  tree next to the store's name and address. The country states that the field is required and what
  shape it takes; it cannot state what a particular shop's number is.
- **It does not implement India's IRP or Japan's authority.** Both go behind the `Fiscalization`
  port, which exists and has a contract suite. Each pack ships the same local-counter implementation
  `countries/zz` proves the shape with, and says so in its own README rather than pretending
  otherwise.
- **It does not decide inter-state versus intra-state GST.** ADR-0104 already recorded that as a
  per-bill fact rather than a per-store one.
- **It does not add a coin denomination list, a symbol, or a currency position.** Each would be a
  field nothing reads today, and a field nothing reads is a field that is wrong by the time
  something does.

## Consequences

- The three packs exist and are selectable by feature. A pilot in Tokyo or Bengaluru is a
  `country-jp` / `country-in` build plus the store's own values — no code.
- `pos_edge` stops passing a literal `None` for cash rounding and starts passing what the store
  published, so an Indian bill settles to the rupee and a Vietnamese one to the thousand.
- The front end's `QUICK_CASH` table stops being the authority and becomes the fallback for a till
  that has not synced a locale yet.
- A fourth country is a copy of `countries/zz`, six constants and a rate table — which is the claim
  ADR-0027 made and this is the first release in which it is true.
