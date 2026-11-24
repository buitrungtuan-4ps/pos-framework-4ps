# ADR-0228 — Comp tax, scale labels and hand weighing stay conservative in each country until legal confirms

**Status** Accepted 2026-11-24 under the owner's instruction to complete phases 7–9 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-24
· Phase 7 (roadmap-v4 row 7), phase 6's carry-overs · Touches tax (AGENTS.md §7) · Relates to
[ADR-0219](0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md),
[ADR-0222](0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md),
[ADR-0223](0223-a-weighed-item-is-priced-by-its-weight.md),
[ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md)

**Open owner decision:** none, beyond the legal and accountant confirmations each item names. Each
item below is **to be confirmed** for `vn`, `jp` and `in`.

## Context

Phase 6 left three questions for each country's legal acceptance suite. How is a comped line taxed?
Which variable-measure (GS1 prefix 20 to 29) layout does a country's labelling scale print? May a
weight be typed by hand? Today a comp lowers the taxable base (ADR-0219 decision 3), only `zz` seeds
a label layout, and `vn`, `jp` and `in` forbid a weight typed by hand.

## Decision

1. **Comp tax.** In `vn`, `jp` and `in`, a comp lowers the taxable base, as a discount does, which is
   what the code already does. No pack says otherwise. Each country's acceptance suite carries a
   golden case that pins it, so a change in law becomes a failing case, not a silent difference.
   Whether a giveaway is taxed at market value (for example as an internal use in Vietnam, or as
   deemed supply in India) is to be confirmed by the accountant. Until they answer, the comp keeps
   its reason and approver on record, so the figure can be rebuilt if the answer differs.
2. **Scale labels.** `vn`, `jp` and `in` seed **no** `scan.variable_measure_layout`, so a store reads
   a 20–29 code as a plain code (`VARIABLE_MEASURE_LAYOUT_NONE`) until it chooses a layout itself.
   Each national GS1 body's layout is to be confirmed. Each pack's acceptance suite asserts that it
   seeds nothing.
3. **Hand weighing.** `vn`, `jp` and `in` keep **forbidding** `weighing.manual_entry`, as law the cloud
   enforces (ADR-0223 decision 3), because a certified scale is the conservative reading of each
   country's metrology law. Each pack's acceptance suite asserts it. Lifting it for a country is a pack
   change after legal confirms.

## Consequences accepted

- No behaviour changes. Three questions become asserted cases in each acceptance suite, and the
  phase 7 exit record lists them as open confirmations.
