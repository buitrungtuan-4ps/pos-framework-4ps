# Vietnam — conformance

**Status** Draft for review · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-21

This lists the legal requirements a person must confirm for Vietnamese stores, and which golden case
in [`tests/conformance.rs`](tests/conformance.rs) exercises each one. The cases run on every
`cargo test` through `pos_contract_tests::country_conformance`.

**The legal acceptance suite** (roadmap-v4 phase 7) is everything this file maps:
`tests/conformance.rs` (cases 1 to 14), `tests/einvoice.rs` and `tests/contract.rs`. It passes when
`cargo test -p pos-country-vn` does. Passing proves that the code does what each row says. It does not
prove the law: every row marked *to confirm* still needs legal or the accountant.

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
| R14 | A receipt template may not leave out the item rows' modifiers; it may leave out the payments, the change line and a second language | `receipt_required_sections`; ADR-0207 decision 3 | unconfirmed: to confirm by legal. The legal body (seller, receipt number, lines, totals, tax lines, a captured buyer) cannot be turned off by any template |
| R15 | An e-invoice symbol is a form digit and six characters (`C` or `K`, the year's two digits, a kind letter, two seller characters); each issuing point has its own seller characters | `einvoice::InvoiceSymbol`; ADR-0225 decision 2 | covered by `tests/einvoice.rs` and case 12 for the shape and case 12 for the shape; which form digit and kind letter a restaurant's cash-register invoices use, and that several symbols may run at once, is to confirm by legal |
| R16 | The sequence starts at 1 under each symbol, is gapless, has at most eight digits, and a new year is a new symbol | `einvoice::next_number`; ADR-0225 decision 2 | covered by `tests/einvoice.rs` and case 11; to confirm by legal |
| R17 | An invoice is an original, an adjustment or a replacement, and a correction names the invoice it corrects; only an adjustment may go down | `einvoice::InvoiceKind`, `check_totals`; ADR-0225 decision 7 | covered by `tests/einvoice.rs`; which error takes which kind is to confirm by legal |
| R18 | An invoice is issued on the store's schedule (`einvoice.issue_moment`): immediately with the settle, which the pack seeds, a set delay after it, in a daily batch, only on request, or not at all | `einvoice::RECOMMENDED_ISSUE_MOMENT`; ADR-0229 decision 1 | covered by `tests/einvoice.rs` and `tests/issue_schedule.rs`; the owner chose the schedule, and whether each mode is lawful for a restaurant is to confirm by legal against Decree 70/2025 |
| R19 | A buyer on an invoice has a name, and a mã số thuế where given is well formed; nothing personal reaches a log through the check | `einvoice::check_buyer`; ADR-0225 decision 4 | covered by `tests/einvoice.rs`; registration is the provider's to check, after the sale |
| R20 | An invoice states each rate once, and its bases and taxes sum to its total | `einvoice::check_totals`; ADR-0225 decision 1 | covered by `tests/einvoice.rs`; whether the cash-rounding line appears on the invoice is to confirm by legal (R5) |
| R21 | A comp lowers the taxable base, as a discount does | ADR-0228 decision 1; ADR-0219 decision 3 | covered by case 8; whether a giveaway is taxed at market value instead is to confirm by the accountant |
| R22 | The pack seeds no variable-measure (GS1 20–29) label layout | ADR-0228 decision 2 | covered by case 9; GS1 Vietnam's layout is to confirm by legal |
| R23 | A weight typed by hand is forbidden; only a scale or its label sets a weight sold by it | `forbidden_settings`; ADR-0228 decision 3, ADR-0223 | covered by case 10; whether a certified scale is required is to confirm by legal |
| R24 | No invoice is issued later than the end of its sale's calendar day: the cloud refuses a delay of a day or more and the edge clamps every schedule to that limit | `einvoice::ISSUE_LIMIT`, `einvoice_issue_limit`; ADR-0229 decision 2 | covered by `tests/issue_schedule.rs`; the limit is the conservative reading and is to confirm by legal |

## Findings: what the current model cannot express

These are findings, not fixes. None is changed by this pack.

- **F1. The e-invoice field set is only partly in the country module.** Since ADR-0225 the module
  carries the symbol, the numbering, the kind, the buyer check and the per-rate totals (R15 to R20).
  The full field list a provider submits, and the provider itself, are still open: R11 stays
  unconfirmed until a provider is chosen.
- **F2. The relief rate cannot be dated in the pack.** R7 lives in configuration by design, so no
  case here can say which rate applies on a given day. A store's `tax` node is where to check it.
