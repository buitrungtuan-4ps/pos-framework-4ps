# `countries/in` — India

Everything in `src/lib.rs` is a constant, per
[ADR-0105](../../docs/adr/0105-a-country-pack-is-values.md).

| | |
|---|---|
| Currency | INR (paise — `amount_minor` is 1/100 of a rupee) |
| Prices quoted | **inclusive** — MRP is inclusive by definition |
| GST | 5 % restaurant service, printed as **CGST 2.5 % + SGST 2.5 %** |
| Cash rounding | ₹1 (Section 170, CGST Act) |
| Notes | ₹10 · ₹20 · ₹50 · ₹100 · ₹200 · ₹500 |
| Numbers | `1,234,567` — see the caveat below |
| Language | `en` |
| Tax code | GSTIN: 15 characters — state code, PAN, entity digit, `Z`, checksum |

## Why India is the pack that needed ADR-0104

A tax invoice for an **intra-state** sale must print CGST and SGST on separate lines: the two halves
go to different governments. Printing their sum is not a terser rendering of the same fact — it is
not a valid invoice. The rate carries its parts, and the framework allocates the tax across them so
they sum to it exactly.

An **inter-state** sale is IGST at the full rate, one component rather than two. That depends on the
buyer's state relative to the seller's, which is a fact about the bill rather than the store, so it
is not a row in this table — the shape carries it and choosing per bill is a deployment's work.

## Alcohol has no row, deliberately

Alcoholic liquor for human consumption is **outside GST**. It attracts state excise and state VAT at
rates each state sets, so there is no national number this pack could publish, and a plausible-looking
one would be wrong in every state but the one it was copied from.

The absence is load-bearing. `rate_for` answers `None`, `pos_core::billing` turns that into
`TaxRateNotConfigured`, and the sale is **refused**. So a store that sells alcohol must publish its
state's rate before it can ring one up — a refusal at the till on the first attempt, rather than a
year of untaxed liquor found by an assessment.

## The lakh caveat

India writes `12,34,567`, grouping by two above the first three. `NumberFormat::digits_per_group` is
a single number and cannot say that, so this pack renders `1,234,567`: wrong-looking rather than
wrong. ADR-0105 records that the fix is a group *pattern* — an additive change with its own visual
consequences, deliberately not bundled into the packs.

## What is still needed to trade

1. **A GSTIN**, issued to a business in a state. Per-store configuration; the pack checks its shape.
2. **The IRP**, for a business above the e-invoicing threshold — the portal returns an IRN and a
   signed QR that must be printed. That is a provider behind the `Fiscalization` port, wrapping the
   offline allocator this pack ships rather than replacing it. Rule 46(b) lets the seller choose its
   own number below the threshold, which is what `fiscalization()` writes.
3. **A state VAT rate for alcohol**, if the store sells any. See above.
