# ADR-0136 — A store publishes how it writes numbers, and each surface knows whose reading it serves

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-22
· Completes a follow-up [ADR-0105](0105-a-country-pack-is-values.md) flagged · Extends
[ADR-0134](0134-a-currency-says-how-many-decimals-it-has.md) and
[ADR-0135](0135-the-console-reads-money-the-way-the-till-does.md) · Relates to
[ADR-0074](0074-localization-and-tax.md)

## The problem

`LocalePack.number_format` — `decimal_separator`, `group_separator`, `digits_per_group` — is
compiled into every country pack and read by nothing. Three surfaces draw grouped money, and each
invents its own answer:

| surface | where | groups | decimal |
|---|---|---|---|
| till | `ui/src/lib/money.ts` | `toLocaleString("en-US")` | `.` |
| receipt | `crates/pos-edge/src/printing.rs` | `,` every three | `.` |
| console | `dashboard/src/lib/format.ts` | the **reader's** browser locale | the reader's |

Vietnam's pack says `1.234.567,50`. The till draws `1,234,567.50`, the paper prints
`1,234,567.50`, and the console draws `1.234.567,50` to a `vi` reader and `1,234,567.50` to an `en`
one. One published fact, four renderings, no readers.

ADR-0134 and ADR-0135 each closed with the same sentence — *typography is a separate decision from
arithmetic* — and the receipt's changelog entry with a third. This is that decision.

## The half nobody has noticed

**The format does not reach a store at all.** `PublishedLocale` in `config_client.rs` carries the
currency, the exponent, the timezone, the cutoff, the tax posture, the rounding increment, the
denominations and the retention period. It does not carry the number format, and neither does the
`locale` node the cloud writes in `http.rs`. The console is served all three fields on
`GET /admin/countries` and ignores them; the edge is never told.

So "honour the published format" is one change on the console and a wire change on the edge, and any
plan that treats them as one size is wrong about the second.

## The question that is genuinely open

Not the receipt, and not the till.

- **The receipt** is a legal document handed to a guest standing in that country — India's Rule 46
  and Japan's qualified invoice both make it one. The store's format wins; there is no second
  reading.
- **The till** is a screen in that shop, read by staff serving that country's guests, showing figures
  that will be on that country's paper. The store's format wins for the same reason, and `en-US` is a
  placeholder that makes the screen disagree with the receipt beside it.

**The console is the open one.** It spans tenants and countries by design: an operator in Ho Chi Minh
City reads a screen whose rows are Bến Thành, Tokyo and Bengaluru.

## Options considered

1. **The store's format always.** A comparison column then carries three typographies —
   `1.234.567`, `1,234,567` — and a column whose rows are spelled differently is harder to scan than
   one that is merely foreign.
2. **The reader's locale always.** Every figure is legible to whoever is reading it, and a price the
   operator is *authoring* for the Tokyo store is shown in a spelling that store's till will never
   use. The Menus editor becomes a screen that shows you something other than what you are making.
3. **By what the operator is doing.** A figure being **authored** — a price in Menus, a rounding
   increment in store settings — follows the **store**, because that field is a preview of the till
   and the paper. A figure the console **reports** — the hub headline, every row in Reports — follows
   the **reader**, because its job is comparison.

## Decision

**Option 3, and the format joins the node a store already receives.**

1. `number_format` joins the `locale` config node beside `currency_exponent` and
   `cash_rounding_increment` — the same shape [ADR-0105](0105-a-country-pack-is-values.md) gave every
   other country value a store carries, and optional on the wire for the reason the exponent is.
2. The **receipt** and the **till** draw with the store's published format, falling back to the
   compiled default when a store has not synced one — the never-blank contract `DEFAULT_CURRENCY`
   and `FALLBACK_EXPONENT` already keep.
3. The **console** draws with the store's format where a figure is authored for one store, and with
   the reader's locale where it reports. No screen mixes the two: Menus and StoreSettings author,
   StoreHub and Reports report.

**`digits_per_group` is not widened here.** ADR-0105 records that India writes `12,34,567`, that a
single number cannot say so, and that fixing it means a group *pattern* — *"a separate, additive
change with its own visual consequences, and bundling it here would hide it."* That reasoning did not
expire. India renders `1,234,567` after this record exactly as before it, and the pattern gets its
own.

## Consequences accepted

- **Every Vietnamese store's receipt changes a second time**, one release after
  [ADR-0134](0134-a-currency-says-how-many-decimals-it-has.md) changed it: `VND 97,900` becomes
  `VND 97.900`. Two changes to one document in two releases is worse than one, and the alternative
  was holding the arithmetic fix until the typography decision was made — which is what ADR-0134
  declined to do, deliberately, because a wrong number is worse than a foreign separator.
- **This is not a `pos-proto` change**, which is worth saying because the two records it extends
  were. `NumberFormat` already sits on `LocalePack`, with a value in every pack; what is missing is
  the *publish*. The `locale` node is JSON the cloud writes in `http.rs` and an edge-local struct
  parses in `config_client.rs` — `pos-proto` holds no schema for it. The field is optional on the
  node under `#[serde(default)]`, which is the additive rule `docs/naming-and-api.md` §11 states, so
  `PROTOCOL_VERSION` does not move; it is the same shape `currency_exponent` took in the same node.

  The record is still required. AGENTS.md §7 asks for one before *changing how money works*, and
  this changes every figure on a Vietnamese till and every figure on its receipt.
- **Japan, India and the reference pack see nothing.** Their format *is* the compiled default; only
  Vietnam's differs today, which is also what makes this cheap to get wrong unnoticed and worth a
  test per surface rather than one.
- **One console screen never draws two typographies**, but two console screens do, and that is
  visible rather than accidental: a store-scoped authoring screen and a fleet-scoped report look
  different on purpose.
- **Dates are not addressed and are in worse shape.** Three console screens call `toLocaleString()`
  with no locale at all while their neighbours pass `locale()`, and no surface has a timezone story.
  That is a different decision with a different answer and it is not this record's.
