# ADR-0104 — A tax rate is a list, and a price may already contain it

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Extends [ADR-0074](0074-localization-and-tax.md) · Relates to [ADR-0027](0027-country-modules.md),
[`pos-spec.md`](../pos-spec.md) §5, [`roadmap-v3.md`](../roadmap-v3.md) B·W4

## The problem

The framework's promise is that a new country is **supplied**, not written: someone clears the legal
registration, fills in values, and a store trades. Two facts about tax break that promise, because
neither can be expressed in the shapes the tree has, and both would need a migration across every
order line ever written if they were added later.

### 1. A tax class resolves to one rate, and India's does not

`TaxRateRow` carries a single `rate`. That is enough for Vietnam (10 % VAT) and enough for Japan
(8 % reduced, 10 % standard — the channel dimension ADR-0074 already gave the table).

It is not enough for India. An Indian tax invoice for an intra-state sale must show **CGST 9 % and
SGST 9 % as separate lines**, not GST 18 % as one. The two halves go to different governments. A
receipt that prints the sum is not a lesser rendering of the same fact — it is not a valid invoice.

The money is identical either way, which is exactly what makes this a shape problem rather than an
arithmetic one: no amount of configuration can split a number the type does not carry.

### 2. `Money::tax_included` exists and nothing can reach it

`pos_proto::money` implements both directions of the calculation, and has since P3. But nothing
selects between them: `pos_core::billing` only ever *adds* tax to a base.

So a store whose menu prices already contain tax cannot be modelled. That store is not exotic:

- **Japan** quotes 税込 (tax-included) prices, and has been required to since 2021.
- **India** quotes MRP, which is inclusive by definition.
- **Vietnam** quotes exclusive, which is the case that already works — and is why the gap survived.

## The decision

**Both facts become data on shapes the tree already publishes, added rather than changed.**

### A rate keeps its total and gains its parts

`TaxRateRow` keeps `rate`, and gains `components`: a list of `(name, rate)` pairs that **must sum to
`rate`**.

```rust
TaxRateRow {
    tax_class_id: standard,
    sales_channel: DineIn,
    rate: TaxRate::from_percent(18),          // what is charged
    components: vec![                          // how it is printed and remitted
        TaxComponent::new("CGST", TaxRate::from_percent(9)),
        TaxComponent::new("SGST", TaxRate::from_percent(9)),
    ],
}
```

`rate` stays the authority on **what the guest pays**, and the components describe **how that total
is composed**. The invariant is checked, not assumed: a table whose components do not sum to their
row's rate is a configuration error, refused where it is authored rather than discovered on an
invoice.

Three properties follow, and each is the reason for the shape:

- **Empty is the common case, not a missing case.** Vietnam and Japan publish no components at all;
  the invoice prints the single rate it always did. A country pays for this feature only if it needs
  it.
- **The money never depends on the components.** Tax is computed from `rate` and then *split* across
  the components, so a mis-authored list can misprint an invoice and can never mischarge a guest.
  The split allocates through `Money::allocate`, so the parts sum exactly to the whole with the
  residual landing on the last — the same primitive the bill-level discount already uses.
- **It is additive on the wire in both directions.** The `tax` node is a bare array of rows; adding
  a `#[serde(default)]` field means an older edge reading a newer publish ignores the key and still
  charges the correct total, and a newer edge reading an older publish sees an empty list and prints
  what it printed before. No `PROTOCOL_VERSION` bump, and no migration.

### The posture rides with the locale, not with the rates

`prices_include_tax` goes on the **`locale`** node, beside `currency_code`, `timezone` and
`cutoff_hour`.

It belongs there because it is a fact about how this store *quotes*, not about any particular rate,
and because the `tax` node is a transparent array with nowhere to put a sibling flag. Turning that
array into an object to make room would be the one thing this ADR is trying to avoid: a wire change
that an older box cannot read, on the node that decides what a guest is charged.

When it is set, a class's `taxable_base` is the price with tax **extracted** rather than the price
with tax **added**:

```
exclusive:  base = 1000        tax = 1000 × 10%      = 100    total = 1100
inclusive:  base = 1000 ÷ 1.1  = 909    tax = 91              total = 1000
```

`BillTotals` reconciles the same way in both postures — `subtotal − discount − comps +
service_charge + tax + rounding = total_due` — which is what keeps one set of assertions honest
across every country.

## What this deliberately does not do

- **It does not make the components a second rate table.** They are a rendering and remittance
  breakdown of one rate. A country that needs two *independently varying* taxes on one line (a
  federal rate plus a municipal one that some cities set themselves) publishes two rows on two
  classes, which the table has always supported.
- **It does not decide inter-state versus intra-state for India.** CGST+SGST versus IGST depends on
  the buyer's state relative to the seller's, which is a fact about *this bill*, not about the
  store's rate table. The shape here carries either; choosing between them per bill is
  `countries/in`'s work, over the `Fiscalization` port that already exists.
- **It does not change rounding.** Tax is still rounded once per class, by the store's configured
  mode, before the components are allocated out of it. Rounding each component independently would
  let the parts miss the whole.

## Consequences

- India becomes expressible: an invoice can print CGST and SGST as the law requires, with the
  amounts summing exactly to the tax charged.
- Japan and any other inclusive-pricing market become expressible, using arithmetic that has been in
  `pos_proto::money` and unreachable since P3.
- Vietnam's behaviour is unchanged, byte for byte: no components, exclusive prices, the same
  `TaxLine` it always produced.
- A country pack now ships a **complete** tax description as data. That is the property this ADR
  exists for: the remaining work to open a market is a registration number and a filled-in form, not
  a pull request.
