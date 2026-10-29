# Japan — conformance

**Status** Draft for review · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-21

This lists the legal requirements a person must confirm for Japanese stores, and which golden case
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
| R1 | 消費税 is 10 % on dine-in food and on all alcohol | `CONSUMPTION_STANDARD`; ADR-0074 | covered by cases 1, 3, 4 and 6 |
| R2 | 軽減税率 is 8 % on food and non-alcoholic drink taken away or delivered | `CONSUMPTION_REDUCED`; `rate_table` | covered by cases 2, 3 and 5 |
| R3 | Alcohol never takes the reduced rate | `rate_table`, comment on alcohol | covered by case 3 |
| R4 | QR ordering is dine-in; the API channel (marketplaces) is delivery. Both are defaults a store may override | `rate_table` doc comment | covered by cases 4 and 5; whether a given store's use of the channel matches is the store's to confirm |
| R5 | 総額表示: menu prices include tax, compulsory since April 2021, so tax is extracted, not added | `prices_include_tax: true`; ADR-0104 | covered by cases 1–6 |
| R6 | There is no cash rounding: the 1-yen coin circulates | `cash_rounding_increment: None` | covered by cases 1–6 |
| R7 | A qualified invoice (適格請求書) carries the seller's 登録番号, `T` and thirteen digits; only the shape is checked here | `is_valid_registration_number`; gate X1; ADR-0106 | covered by case 8 for the shape; printing it on the receipt is the store profile's and is unconfirmed here |
| R8 | A qualified invoice shows the tax per rate | `fiscalization()` doc comment | covered by case 3, where each rate is its own tax line |
| R9 | The consumption tax on a qualified invoice is rounded once per rate per invoice | raised as a requirement to confirm; not in the module | **unconfirmed**: to confirm by legal. See F1: the model rounds once per class |
| R10 | The invoice serial is the seller's own; no authority allocates it | `fiscalization()` doc comment | covered by case 7 |
| R11 | Two invoice ranges never share a number | `fiscalization()`; `pos_country::offline` | covered by case 7 |
| R12 | Any other field a qualified invoice must carry (for example the buyer, the date, the items) | none in the module | unconfirmed: to confirm by legal |
| R13 | Personal data retention, 365 days by default | `default_retention_days`; gate X3 | unconfirmed: the module says somebody must confirm it |

## Findings: what the current model cannot express

These are findings, not fixes. None is changed by this pack.

- **F1. Tax rounds per class, not per rate.** `pos_core::billing` rounds each tax class's tax once
  (ADR-0028). Two classes at the same rate, such as dine-in food and alcohol at 10 %, give two tax
  lines and two roundings. Case 6 shows it: ¥1,005 of each gives 91 + 91 = ¥182, where one rounding
  of the 10 % total would give ¥183. If R9 is confirmed, the model cannot express it today: it needs
  either one class per rate in Japan, or a per-rate rounding step. That is a decision for a record,
  not for this pack.
- **F2. The qualified-invoice field set is not in the country module.** The module ships rates,
  posture and the registration number's shape. The document's other fields are the receipt
  template's and the store profile's (ADR-0106). This pack cannot check R12.
