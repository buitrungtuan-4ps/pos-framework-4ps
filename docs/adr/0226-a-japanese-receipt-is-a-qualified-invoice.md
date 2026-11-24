# ADR-0226 — A Japanese receipt is a simplified qualified invoice, taxed and rounded once per rate

**Status** Accepted 2026-11-24 under the owner's instruction to complete phases 7–9 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-24
· Phase 7 (roadmap-v4 row 7, roadmap-v3 B10.1) · Changes how tax is rounded (AGENTS.md §7) ·
Relates to [ADR-0028](0028-settlement-and-payment-invariant.md),
[ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0106](0106-the-store-is-a-legal-person.md),
[ADR-0105](0105-a-country-pack-is-values.md), ADR-0225

**Open owner decision:** none beyond the legal confirmations. Every statement of Japanese law below
is **to be confirmed by legal or the accountant**. The pack already documents R1 to R14 in
`countries/jp/CONFORMANCE.md`, and this record does not settle them.

## Context

Japan's invoice system (インボイス制度, from October 2023) asks a registered seller to issue a
**qualified invoice** (適格請求書). A restaurant may issue the **simplified** form (適格簡易請求書),
which may leave out the buyer's name. The pack carries the rates (10 % standard, 8 % reduced for food
taken away), tax-inclusive prices, no cash rounding, and the shape of the 登録番号 (`T` and thirteen
digits). Its finding F1 is the gap: `pos_core::billing` rounds tax **once per class**, so two classes
at 10 % round twice, while a qualified invoice is understood to round **once per rate per invoice**
(R9). Nothing in Japan is submitted to an authority, so no provider and no port is needed.

## Options considered

1. **One tax class per rate in Japan.** No code change, but a menu loses its alcohol class, and a
   report by class means nothing.
2. **A per-rate rounding step the country chooses** (chosen). The pack says `tax_rounding_scope`:
   `PER_CLASS` (the default, every other country unchanged) or `PER_RATE`.
3. **Round per line.** Explicitly not allowed on a qualified invoice, as we understand it.

## Decision

1. **Rounding once per rate.** The country pack gains `tax_rounding_scope`, a value (ADR-0105),
   not a setting. Japan's is `PER_RATE`. Then `billing::assemble` sums each rate's base across
   classes, rounds the tax once per rate, and gives each class its share of that rate's tax by the
   allocation `Money::allocate` already uses, so the class lines still sum to the rate's tax. The
   rounding stays the framework's half up (ADR-0028). Many Japanese sellers round down (切り捨て);
   whether Japan needs a rounding mode of its own is to be confirmed, and one is added only if legal
   says so.
2. **The receipt is the simplified qualified invoice.** It carries the seller's name and 登録番号
   (ADR-0106's seller record), the date, the items with the reduced-rate items marked (※), and for
   each rate the tax-inclusive total and the tax. The receipt template's legal body includes them,
   and no template can turn them off (ADR-0207). A full qualified invoice with the buyer's name is
   the same receipt with a captured buyer, kept apart as in ADR-0225 decision 4.
3. **Translations:** every label on it is a translation key with a `ja` value (領収書, 適格簡易請求書,
   登録番号, 8%対象, 10%対象, 内消費税, ※は軽減税率対象). The till's own strings get a `ja` column.
4. **JPY denominations** stay the pack's: a guest hands over notes of ¥1,000, ¥5,000 and ¥10,000, a
   drawer is counted from the ¥1 coin up, and there is no cash rounding. Whether the ¥2,000 note
   belongs in the count is the store's to set.
5. **Numbering:** the receipt's series and number (ADR-0152) are the invoice's. No authority
   allocates them, as R10 records.

## Configuration

- **Pack value (law):** `tax_rounding_scope = PER_RATE`, for Japan only.
- **Store profile:** the seller's registration number, as today.

## Consequences accepted

- `billing::assemble` gains one branch on a pack value. Every country except Japan computes exactly
  as before, and the existing golden cases prove it.
- Japan's conformance case 6 changes its expected tax, from ¥91 + ¥91 to one rounding of the 10 %
  total. That is the point of this record, and it is recorded as a legal confirmation still owed.
