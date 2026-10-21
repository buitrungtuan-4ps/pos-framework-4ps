# India — conformance

**Status** Draft for review · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-21

This lists the legal requirements a person must confirm for Indian stores, and which golden case in
[`tests/conformance.rs`](tests/conformance.rs) exercises each one. The cases run on every
`cargo test` through `pos_contract_tests::country_conformance`.

How to read the status:

- **covered by case N**: the module's value is what case N computes with. The case proves what the
  code does. It does not prove that this is what the law requires; that still needs a person.
- **unconfirmed**: nothing in this pack exercises it, or the law behind it needs confirming.

Every requirement comes from [`src/lib.rs`](src/lib.rs), [`README.md`](README.md) or an ADR. Anything
beyond those is marked **to confirm by legal**. The line prices in the cases are illustrative.

| # | Requirement | Source | Status |
|---|---|---|---|
| R1 | Restaurant GST is 5 %, without input tax credit (Notification 11/2017 as amended) | `GST_RESTAURANT` | covered by cases 1–4 |
| R2 | An intra-state sale prints CGST and SGST on separate lines, 2.5 % each | `GST_HALF`; ADR-0104 | covered by cases 1–4 |
| R3 | The CGST and SGST amounts add up exactly to the tax | `Money::allocate`; ADR-0104 | covered by cases 2, 3 and 4, where the tax is odd |
| R4 | An inter-state sale is IGST at the full 5 %, one component, decided by the buyer's state | `rate_table` doc comment | **unconfirmed**. See F1: nothing chooses IGST per bill |
| R5 | MRP is inclusive, so tax is extracted from the price (Legal Metrology (Packaged Commodities) Rules) | `prices_include_tax: true` | covered by cases 1–4 |
| R6 | The invoice total rounds to the nearest rupee (CGST Act section 170) | `CASH_ROUNDING` | covered by cases 2 and 4; that rounding is half up is the framework's choice and is to confirm by legal |
| R7 | Alcoholic liquor is outside GST: state excise and state VAT, so there is no national row | `rate_table`, "Why alcohol is missing" | covered by case 5: the sale is refused until the store publishes its state's rate |
| R8 | A restaurant in a hotel with a room tariff above ₹7,500 charges 18 % | `GST_RESTAURANT` doc comment | unconfirmed: a store's `tax` node, not this pack |
| R9 | The invoice number is chosen by the seller, at most sixteen characters, letters, digits, `/` and `-`, unique within a financial year (Rule 46(b)) | `fiscalization()` | covered by case 6 for length, characters and no repeat across ranges |
| R10 | A buyer's GSTIN is fifteen characters with the PAN inside; only the shape is checked | `is_valid_gstin` | covered by case 7 |
| R11 | Above the e-invoicing threshold, the invoice is registered with the IRP, which returns an IRN and a signed QR | module doc; gate X2 | unconfirmed: a provider, not this pack |
| R12 | Any other field a tax invoice must carry (for example HSN or SAC codes, place of supply) | none in the module | unconfirmed: to confirm by legal. See F2 |
| R13 | Personal data retention, 365 days by default (DPDP Act 2023) | `default_retention_days` | unconfirmed: the module says somebody must confirm it against the notice and consent record |

## Findings: what the current model cannot express

These are findings, not fixes. None is changed by this pack.

- **F1. IGST cannot be chosen per bill.** The rate table is keyed by tax class and channel, per
  store. Whether a sale is intra-state (CGST + SGST) or inter-state (IGST) depends on the buyer's
  state against the seller's, which the module says is a fact about the bill. Nothing in the bill
  model carries a place of supply or picks IGST, so R4 cannot be expressed today. The component
  names accept `IGST` and `UTGST`; nothing selects them.
- **F2. No HSN or SAC code, and no place of supply.** No item or bill field carries them. If legal
  confirms they are required on a tax invoice (R12), the model needs a field for each.
- **F3. Indian digit grouping.** India writes 12,34,567; `NumberFormat` has one group size, so the
  module renders 1,234,567. The module and ADR-0105 already record this.
