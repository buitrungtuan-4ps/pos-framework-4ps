# ADR-0134 — A currency says how many decimals it has, and a missing answer is not zero

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-22
· Extends [ADR-0105](0105-a-country-pack-is-values.md) · Relates to
[ADR-0129](0129-a-receipt-itemises-what-was-sold.md), [`naming-and-api.md`](../naming-and-api.md) §4

## The problem

Money is an integer in the currency's minor unit, and floating point is banned at every layer
([`naming-and-api.md`](../naming-and-api.md) §4). Turning that integer back into something a person
reads needs one more fact — how many decimal places the currency has — and **nothing in this tree
holds it**.

`pos-proto` does not. `CurrencyCode` is three letters; the exponent appears only in prose on the
constants (*"the paisa — two decimal places, so `100` is one rupee"*), where no code can read it.

The front end holds a three-row guess:

```ts
const MINOR_DIGITS: Record<string, number> = { VND: 0, JPY: 0, USD: 2 };
//                          …
const digits = MINOR_DIGITS[m.currency_code] ?? 0;
```

**The `?? 0` is the defect, not the missing rows.** VND and JPY genuinely are zero-decimal, so the
default is *correct* for the two currencies this framework started with and silently wrong for every
other one. A missing entry is indistinguishable from a real answer.

`countries/in` denominates in **INR**, whose minor unit is the paisa. It is a supplied, tested
country pack today. For a store on it:

| surface | code | ₹261.45 becomes |
|---|---|---|
| the till's display | `formatMoney` | `INR 26,145` |
| the till's money **input** | `parseWhole` multiplies by `10 ** digits` | a cashier typing a ₹150,000 discount enters ₹1,500 |
| the printed receipt | `format_money` in `pos-edge` | `INR 26145` |

The input row is the one that is not cosmetic. And the receipt is a **tax document** in both markets
that have one: India's Rule 46 and Japan's qualified invoice are already cited in this tree.

The receipt's own comment says the omission is deliberate — *"the decimal place is a locale question
and a receipt that invented one would be wrong in half the countries this framework targets"*.
That was the right call when there was nothing to read. It is not a place to stay.

[ADR-0105](0105-a-country-pack-is-values.md) §2 moved the **quick-cash denominations** out of
`ui/src/lib/money.ts` for exactly this reason: *"which notes a guest can hand over is a fact about a
country's cash, not about a screen."* `MINOR_DIGITS` sits a hundred lines above the table it moved,
and was missed.

## Options considered

1. **Add the missing rows to `MINOR_DIGITS`.** One line, and it reproduces on the fortieth market
   what it fixes on the fourth: the next currency is still silently zero-decimal, and the receipt and
   the cloud console still hold no answer at all.
2. **A complete ISO 4217 table in `pos-proto`.** The exponent *is* an ISO fact — no store may
   disagree that INR has two — so this is defensible. But it makes the framework the authority on a
   list that changes without it, and ADR-0105 has already ruled that a market's facts are supplied
   rather than compiled in. It also leaves the same silent default at the bottom.
3. **A published value, and no silent default.** The country pack states its exponent beside the
   cash increment and the denominations it already states; it reaches the till on the `locale` node
   and `GET /api/locale`, as they do.

## Decision

**Option 3, with the framework's table kept only as a named fallback and a missing answer made
loud.**

- `LocalePack` gains `currency_exponent`, beside `cash_rounding_increment` and
  `cash_denominations`, as a **required** field. Every supplied pack must then state it or fail to
  compile — a struct literal missing a field is an error, which is a stronger gate than any check
  `xtask` could run and needs no new check at all. On the **wire** the same value is optional with a
  default, because a cloud that predates it must still publish a locale node an edge can apply; the
  two obligations are different and the two types say so.
- It rides the `locale` config node and `GET /api/locale` to the till, which stops holding a currency
  table: `MINOR_DIGITS` is deleted, not extended.
- `pos-edge` formats the receipt with it, replacing the raw minor units.
- A store that has not synced keeps trading (the never-blank contract), on a **named** compiled-in
  fallback for the currencies this framework supplies packs for — not on `?? 0`. A currency the
  fallback does not carry is a `warn` at boot naming the currency, in the same shape as the missing
  printing font: an operator learns it from a log line rather than from a receipt.

## Consequences accepted

- **A schema addition, and no protocol bump.** `currency_exponent` is additive and optional on the
  wire, which `naming-and-api.md` §11 says is the ordinary case — `PROTOCOL_VERSION` moves only for a
  break. An edge that predates it falls back as above, so an older store keeps the behaviour it has.
- **One more thing a new country must state.** That is ADR-0105's bargain, and the required field
  makes forgetting it a build failure rather than a wrong receipt.
- **The receipt's printed form changes** for every store, including Vietnam, where `VND 97900`
  becomes a figure with a thousands separator. That is a user-visible change to a document, so it
  lands with a CHANGELOG upgrade note.
- **This does not make the framework an authority on ISO 4217.** The fallback covers the currencies
  we ship packs for and says so; anything else is supplied or is flagged.
- **`Money` itself does not change.** The integer, its arithmetic and its wire form are untouched —
  this is about rendering and parsing at the edges, which is the only place a decimal point has ever
  belonged.
