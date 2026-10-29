# `countries/vn` — Vietnam

**Layer** `country` · **Owners** @maintainers-architecture

Everything in `src/lib.rs` is a constant. That is the point:
[ADR-0105](../../docs/adr/0105-a-country-pack-is-values.md) is the claim that a market is
**supplied**, and this directory is what supplying one looks like.

| | |
|---|---|
| Currency | VND (no minor unit — `amount_minor` is a whole đồng) |
| Prices quoted | **exclusive** — the familiar `++` |
| VAT | 10 % on every channel, food and alcohol alike |
| Cash rounding | 1,000 ₫ |
| Notes | 10k · 20k · 50k · 100k · 200k · 500k |
| Numbers | `1.234.567,89` |
| Language | `vi` |
| Tax code | mã số thuế: ten digits, optionally `-` and three more for a branch |
| New store | `shift.variance_reason_minor` = 20,000 ₫: a drawer closing over or short by more asks a reason |

The last row is a **setting default** ([ADR-0183](../../docs/adr/0183-a-country-pack-seeds-a-new-stores-settings.md)): a value a new Vietnamese store is given when the
console applies its new-store values, before the register's own presets, and only where the store
does not set the value already. The owner confirmed the figure on 2026-10-29. Any store, group,
brand or tenant can set another value; the pack only removes the empty start, where a drawer closed
82,450 ₫ short in one tap.

## The 8 % question

Vietnam's standing VAT rate is 10 %. A 2-point relief has applied to eligible goods and services
since Resolution 43/2022 and has been extended more than once, so a great many restaurants charge
**8 %** today.

This pack publishes 10 % and leaves the relief to the store's `tax` node. A dated, repeatedly
extended concession is exactly what configuration is for: a store that has to wait for a release to
follow a decree is a store the framework has failed.

## What is still needed to trade

The legal half, which no pull request can close:

1. **An e-invoice provider.** Vietnam's e-invoicing runs through a licensed provider's API. This pack
   ships the offline allocator every country shares (`pos_country::offline`), which satisfies the
   whole `Fiscalization` contract — including issuing with the line down — but contacts nobody.
   Wiring a provider replaces the submission and keeps the offline path.
2. **The store's own values.** Its tax code, its address, its registered name. Configuration, not
   code.

## What belongs here, and what does not

The line is [ADR-0027](../../docs/adr/0027-country-modules.md): **this module ships what the law
says; the configuration tree overrides it.** The store's timezone is not here (it is a store's
fact), and neither is whether a tax code is *registered* (that is a network call, and keeping it out
is what lets a cashier take a corporate customer's code offline).

## Conformance

[`CONFORMANCE.md`](CONFORMANCE.md) lists the legal requirements a person must confirm, and the golden
bills in [`tests/conformance.rs`](tests/conformance.rs) that exercise them.

## Depends on / used by

- **Depends on** (workspace crates): `pos-country`, `pos-ports`, `pos-proto`; in tests, `pos-contract-tests`, `pos-fakes`.
- **Used by** (workspace crates): `pos-cloud`, `pos-edge`.
