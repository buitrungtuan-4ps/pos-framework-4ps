# `countries/jp` — Japan

Everything in `src/lib.rs` is a constant, per
[ADR-0105](../../docs/adr/0105-a-country-pack-is-values.md).

| | |
|---|---|
| Currency | JPY (no minor unit) |
| Prices quoted | **inclusive** — 総額表示, compulsory since April 2021 |
| Consumption tax | 10 % standard · **8 % reduced** on food carried out |
| Cash rounding | none — the 1-yen coin circulates |
| Notes | ¥1,000 · ¥5,000 · ¥10,000 |
| Numbers | `1,234,567` |
| Language | `ja` |
| Tax code | 登録番号: `T` and thirteen digits |

## Why Japan is the pack that exercises the whole table

|  | dine-in · QR | takeaway · delivery · API |
|---|---|---|
| food, soft drink | 10 % | **8 %** |
| alcohol | 10 % | 10 % |
| exempt | 0 % | 0 % |

Both dimensions carry weight here, which is why the framework has both:

- **Channel**, because the same onigiri is 8 % carried out and 10 % eaten in — a dine-in sale is a
  service, and only food *for takeaway* takes the relief.
- **Class**, because alcohol is excluded from the relief. Takeaway beer is 10 % while the takeaway
  food beside it is 8 %, and one rate per channel could not say that.

QR ordering is treated as dine-in (a guest at a table) and the API channel as delivery (a
marketplace). Both are defaults a store overrides if it uses the channel for something else.

## What is still needed to trade

1. **A qualified-invoice registration number** (適格請求書発行事業者登録番号), issued to a business by
   application. It is a per-store value in the configuration tree — the pack checks its shape and
   cannot know a particular shop's number.
2. **The registration number and per-rate breakdown printed on the document.** A qualified invoice
   must show the seller's number and the 8 %/10 % totals separately. The tax data is there
   ([ADR-0104](../../docs/adr/0104-multi-component-and-inclusive-tax.md)); putting it on the paper is
   the receipt template's work.

Japan's authority allocates **no** invoice numbers — a seller chooses its own serial — so the
locally allocated range here is the correct answer rather than a stand-in.
