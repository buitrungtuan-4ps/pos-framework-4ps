# ADR-0135 — The console reads and writes money the way the till does

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-22
· Extends [ADR-0134](0134-a-currency-says-how-many-decimals-it-has.md) · Completes a follow-up
[ADR-0082](0082-catalog-and-layout-rebuild.md) flagged · Relates to
[ADR-0074](0074-localization-and-tax.md)

## The problem

[ADR-0134](0134-a-currency-says-how-many-decimals-it-has.md) gave the till and the printed receipt
the currency's exponent, and both stopped guessing. The **console** still guesses, on a third
surface, and it guesses the same way for the same reason.

```ts
// dashboard/src/lib/format.ts
export function formatMoney(money: Money): string {
  return `${new Intl.NumberFormat(locale()).format(money.amount_minor)} ${money.currency_code}`;
}
```

`amount_minor` is formatted directly. For a store on INR that means:

- **`StoreHub`** shows the day's **net revenue** as a headline figure — ₹261.45 reads `26,145 INR`.
- **`Reports`** formats every figure in the sales rollup the same way.

Nobody decided that a revenue report shows paise. The rationale is written down in the component
next door, and it is an assumption rather than a position:

> `MoneyField` … *"VND (v1) has no minor part, and other currencies are authored in their minor
> units, the same convention `formatMoney` reads back."*

That is the **same** "Vietnam, v1" assumption that produced `?? 0` in the till and raw minor units on
the receipt. ADR-0134 retired it; the console did not hear.

**This is not a reversal of ADR-0082.** That record names its own gap: *"Deliberately not built: a
backend read of the store's published currency to prefill the field … wiring the field to the
store's actual published currency is a flagged follow-up."* The convention was contingent on a read
that did not exist. It exists now — `GET /admin/countries` carries `currency_exponent`, and both
`StoreSettings` and `Menus` already load that list.

## What is genuinely new, and needs deciding

Display is a defect and needs no permission. **Authoring is a change to what an operator types.**

`MoneyField` edits the integer directly, so a price of ₹261.45 is typed `26145`. Reading it as money
means typing `261.45`. That is better — it is what the price *is*, and it removes an arithmetic step
from every price in a two-decimal country — but it is a habit, and habits are worth a record.

The two cannot be separated. `Menus` displays a price through `formatMoney` and edits it through
`MoneyField` **on the same screen**: fixing display alone puts `261.45` next to a field showing
`26145`, which is worse than either.

## Options considered

1. **Display only.** Fixes the reports. Breaks the Menus screen's internal agreement.
2. **Nothing, and document the convention harder.** Leaves an Indian operator reading revenue in
   paise, on the screen the business is run from.
3. **Both, together.** The console reads and writes money as money, using the published exponent.

## Decision

**Option 3.** The console resolves the exponent from `GET /admin/countries` and applies it to both
directions. `formatMoney` takes the exponent as an argument, as the till's does since
[ADR-0134](0134-a-currency-says-how-many-decimals-it-has.md), so it stays pure and cannot be called
without one.

One difference from the till is deliberate: the console holds a **currency → exponent map**, not a
single value. A till serves one store in one currency; the console spans every country the platform
has a pack for, and a figure it draws belongs to whichever store it came from.

## Consequences accepted

- **Nothing stored changes.** `amount_minor` is the wire and database form either way; only what the
  operator reads and types moves. A price authored before this release is the same integer after it.
- **Operators in two-decimal countries type differently**, which is a user-visible change and lands
  with a CHANGELOG upgrade note. Vietnam and Japan see nothing: their exponent is zero, and the
  field and the figure are what they were.
- **A currency the console has not loaded a country for falls back to zero**, which is the behaviour
  it has today. That window is the app's first read, and a store always belongs to a country the
  platform ships a pack for.
- **Typography is still not addressed.** `LocalePack.number_format` is published and read by nothing
  — the till groups `en-US`, the console groups by the reader's browser locale, and Vietnam's
  `120.000,50` appears nowhere. Grouping is a separate decision from arithmetic, it has to answer
  who wins on a screen an operator in another country is reading, and it is not this record's.
