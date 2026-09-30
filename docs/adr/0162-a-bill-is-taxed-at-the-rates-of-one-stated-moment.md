# ADR-0162 — A bill is taxed at the rates of one stated moment, and the country says which

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-30
· Relates to [ADR-0028](0028-settlement-and-payment-invariant.md),
[ADR-0104](0104-multi-component-and-inclusive-tax.md), [ADR-0105](0105-a-country-pack-is-values.md)

## The problem

A line records the tax rate it was rung at (`tax_rate` on `sales.order_line.added`), but no total is
computed from it. The check read, the pre-bill and the settle all call `billing::assemble` with the
store's **current** rate table (`bill_input` in `crates/pos-edge/src/app.rs`). So the moment that
decides a bill's tax is whenever the till next asks:

- a rate the cloud publishes during a meal changes a bill whose pre-bill the guest already has in
  hand;
- a bill opened before a statutory change and paid after it is taxed at the new rate, whatever the
  pre-bill said;
- two reads a minute apart can disagree, and nothing records which rates a settled bill used.

Vietnam's VAT reductions have moved a restaurant's rate between 10% and 8% at midnight more than
once since 2022, so the last case is not hypothetical. The event's own field says the rate is
"captured rather than looked up later"; the bill looks it up later.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep reading the current table | The rate is set by timing, and the record cannot show which applied |
| B | Tax each line at the rate it was rung at | Wrong at a statutory change: the invoice date is what the law keys on, not the order time |
| C | **Freeze the rates when the bill opens, and let the country move that moment to settlement** | One snapshot on an event, one country value |

## Decision (proposed)

Option **C**. The owner approved freezing at bill open on 2026-09-30.

1. **A bill keeps the rates it opened with.** When a bill opens, the rates for its lines' classes
   and its channel are recorded on `billing.bill.opened`, additively. The check read, the pre-bill
   and the settle use those rates and nothing else, so the pre-bill is the bill. A bill voided and
   re-opened takes the rates of its new opening.
2. **The country states the moment.** A country pack value, `tax_point`: `BILL_OPENED` (the
   default) or `SETTLED`. At `SETTLED` the rates are taken again at settle, recorded on
   `billing.bill.settled`, and a bill whose rates changed since the pre-bill cannot settle until
   the pre-bill is printed again. A store never mixes the two.
3. **The settled bill says what it used.** `billing.bill.settled` carries the rate for each tax
   class it taxed (with roadmap-v3 B4.1's `tax_lines`), so a receipt can be reprinted and an
   e-invoice issued from the log alone.
4. **The line's own rate stays informational.** It shows the till the rate at the moment of the
   sale and is not used in any total.

## Before this is accepted

- **Accounting confirms the Vietnamese tax point.** The invoice rules (Decree 123/2020, Art. 9) tie
  a service's invoice to when it is completed or paid for, which for a restaurant is the settle.
  If accounting reads it that way, Vietnam's value is `SETTLED` and `BILL_OPENED` serves markets
  that key on the order. Until accounting answers, Vietnam's pack says `SETTLED`, the behaviour
  closest to today's.
- The owner confirms that a pre-bill printed again is an acceptable price for `SETTLED` on the rare
  night a rate changes mid-meal.

## Consequences accepted

- **`pos-core`, `pos-proto` and the event schema change**: `billing::assemble` takes the bill's
  recorded rates, and two events gain fields, additively. That needs the owner's review under
  AGENTS.md §6, and `docs/snapshots/events.txt` grows.
- **A publish no longer changes an open bill**, which is the point. A rate typo fixed during
  service reaches the next bill, not the one on the table.
- **A settled bill before this change** has no recorded rates; reports read them as "not recorded"
  rather than guessing from today's table.
