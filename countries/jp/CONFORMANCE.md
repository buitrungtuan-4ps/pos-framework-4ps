# Japan — conformance

**Status** Draft for review · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-21

This lists the legal requirements a person must confirm for Japanese stores, and which golden case
in [`tests/conformance.rs`](tests/conformance.rs) exercises each one. The cases run on every
`cargo test` through `pos_contract_tests::country_conformance`.

**The legal acceptance suite** (roadmap-v4 phase 7) is everything this file maps:
`tests/conformance.rs` (cases 1 to 11) and `tests/contract.rs`. It passes when `cargo test` on this
crate does. Passing proves that the code does what each row says. It does not prove the law: every
row marked *to confirm* still needs legal or the accountant.

How to read the status:

- **covered by case N**: the module's value is what case N computes with. The case proves what the
  code does. It does not prove that this is what the law requires; that still needs a person.
- **unconfirmed**: nothing in this pack exercises it, or the law behind it needs confirming.

Every requirement comes from [`src/lib.rs`](src/lib.rs), [`README.md`](README.md) or an ADR. Anything
beyond those is marked **to confirm by legal**. The line prices in the cases are illustrative.

| # | Requirement | Source | Status |
|---|---|---|---|
| R1 | 消費税 is 10 % on dine-in food and on all alcohol | `CONSUMPTION_STANDARD`; ADR-0074 | covered by cases 1, 3, 4 and 6 |
| R2 | 軽減税率 is 8 % on food and non-alcoholic drink taken away or delivered | `CONSUMPTION_REDUCED`; `rate_table` | covered by cases 2, 3 and 5 |
| R3 | Alcohol never takes the reduced rate | `rate_table`, comment on alcohol | covered by case 3 |
| R4 | QR ordering is dine-in; the API channel (marketplaces) is delivery. Both are defaults a store may override | `rate_table` doc comment | covered by cases 4 and 5; whether a given store's use of the channel matches is the store's to confirm |
| R5 | 総額表示: menu prices include tax, compulsory since April 2021, so tax is extracted, not added | `prices_include_tax: true`; ADR-0104 | covered by cases 1–6 |
| R6 | There is no cash rounding: the 1-yen coin circulates | `cash_rounding_increment: None` | covered by cases 1–6 |
| R7 | A qualified invoice (適格請求書) carries the seller's 登録番号, `T` and thirteen digits; only the shape is checked here | `is_valid_registration_number`; gate X1; ADR-0106 | covered by case 8 for the shape; printing it on the receipt is the store profile's and is unconfirmed here |
| R8 | A qualified invoice shows the tax per rate | `fiscalization()` doc comment | covered by case 3, where each rate is its own tax line |
| R9 | The consumption tax on a qualified invoice is rounded once per rate per invoice | `tax_rounding_scope: PerRate`; ADR-0226 decision 1 | covered by case 6, where two classes at 10 % round once to ¥183; that this is the law is to confirm by legal |
| R10 | The invoice serial is the seller's own; no authority allocates it | `fiscalization()` doc comment | covered by case 7 |
| R11 | Two invoice ranges never share a number | `fiscalization()`; `pos_country::offline` | covered by case 7 |
| R12 | Any other field a qualified invoice must carry (for example the buyer, the date, the items) | none in the module | unconfirmed: to confirm by legal |
| R13 | Personal data retention, 365 days by default | `default_retention_days`; gate X3 | unconfirmed: the module says somebody must confirm it |
| R14 | A receipt template may not leave out the item rows' modifiers; it may leave out the payments, the change line and a second language | `receipt_required_sections`; ADR-0207 decision 3 | unconfirmed: to confirm by legal. The legal body (seller, receipt number, lines, totals, tax lines, a captured buyer) cannot be turned off by any template |
| R15 | A comp lowers the taxable base, as a discount does | ADR-0228 decision 1; ADR-0219 decision 3 | covered by case 9; whether a giveaway is taxed at market value instead is to confirm by the accountant |
| R16 | The pack seeds no variable-measure (GS1 20–29) label layout | ADR-0228 decision 2 | covered by case 10; GS1 Japan's layout is to confirm by legal |
| R17 | A weight typed by hand is forbidden; only a scale or its label sets a weight sold by it | `forbidden_settings`; ADR-0228 decision 3 | covered by case 11; whether a certified scale (検定) is required is to confirm by legal |

## Findings: what the current model cannot express

These are findings, not fixes. None is changed by this pack.

- **F1. Closed by ADR-0226.** Tax used to round once per class, so two classes at 10 % rounded
  twice (¥91 + ¥91). The pack now says `PerRate`, so the bill rounds once per rate and shares the
  tax back across the classes. Case 6 shows ¥183. The edge reads the scope from the country it was
  built for; every other country still rounds per class.
- **F2. The qualified-invoice field set is not in the country module.** The module ships rates,
  posture and the registration number's shape. The document's other fields are the receipt
  template's and the store profile's (ADR-0106). This pack cannot check R12.
