# Vietnam — conformance

**Status** Draft for review · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-21

This lists the legal requirements a person must confirm for Vietnamese stores, and which golden case
in [`tests/conformance.rs`](tests/conformance.rs) exercises each one. The cases run on every
`cargo test` through `pos_contract_tests::country_conformance`.

How to read the status:

- **covered by case N**: the module's value is what case N computes with. The case proves what the
  code does. It does not prove that this is what the law requires; that still needs a person.
- **unconfirmed**: nothing in this pack exercises it, or the law behind it needs confirming.

Every requirement comes from [`src/lib.rs`](src/lib.rs), [`README.md`](README.md) or an ADR. Anything
beyond those is marked **to confirm by legal**. The line prices in the cases are illustrative.

| # | Requirement | Source | Status |
|---|---|---|---|
| R1 | VAT is 10 % on food on every channel | `VAT_STANDARD`; `docs/pos-spec.md` §5 | covered by cases 1–5 |
| R2 | Alcohol takes the same VAT as food. Special consumption tax is inside the purchase price and not a line on the guest's bill | `rate_table`, comment on alcohol | covered by case 2; the statement about special consumption tax is to confirm by legal |
| R3 | An exempt line is taxed at 0 % | `rate_table`, `tax_class::exempt()` | covered by case 3 |
| R4 | Menu prices are quoted before VAT (`++`) | `prices_include_tax: false`; ADR-0104 | covered by cases 1–5 |
| R5 | Cash totals round to 1,000 ₫, half up | `CASH_ROUNDING`; `pos_core::billing::CASH_ROUNDING` | covered by cases 1–5; whether an invoice may show a rounded payable amount is to confirm by legal |
| R6 | Tax rounds once per class, half up, to the whole đồng | `currency_exponent: 0`; ADR-0028 | covered by case 5; whether half up is the required method is to confirm by legal |
| R7 | The 2-point relief (8 %) under Resolution 43/2022 and its extensions is a store's `tax` node, not this pack | `VAT_STANDARD` doc comment; `README.md` "The 8 % question" | unconfirmed: which goods it covers and whether it is in force on a given date is to confirm by legal |
| R8 | A buyer's mã số thuế is ten digits, or ten and a three-digit branch; only the shape is checked | `is_valid_mst` | covered by case 7 |
| R9 | An e-invoice number is the provider's form and serial plus an eight-digit sequence; offline, the pack writes the same shape | `fiscalization()` | covered by case 6 for the offline shape; the real serial comes from a licensed provider and is unconfirmed |
| R10 | Two invoice ranges never share a number | `fiscalization()`; `pos_country::offline` | covered by case 6 |
| R11 | The fields a Vietnamese e-invoice must carry, and its submission | ADR-0156 (Proposed) | unconfirmed: to confirm by legal and the chosen provider |
| R12 | The moment a bill's tax rate is fixed (the tax point) | ADR-0162 (Proposed, awaits accounting) | unconfirmed |
| R13 | Personal data retention, 365 days by default | `default_retention_days`; gate L1, L2, L6 | unconfirmed: the module says this is a starting value the operator must confirm |

## Findings: what the current model cannot express

These are findings, not fixes. None is changed by this pack.

- **F1. The e-invoice field set is not in the country module.** The module ships the number format
  and nothing else. The fields an e-invoice carries, and the provider that submits it, are ADR-0156's
  (Proposed). This pack cannot check R11 until that lands.
- **F2. The relief rate cannot be dated in the pack.** R7 lives in configuration by design, so no
  case here can say which rate applies on a given day. A store's `tax` node is where to check it.
