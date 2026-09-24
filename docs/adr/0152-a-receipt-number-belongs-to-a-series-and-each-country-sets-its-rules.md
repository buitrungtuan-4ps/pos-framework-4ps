# ADR-0152 — A receipt number belongs to a series, and each country sets the series' rules

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0025](0025-receipt-number-authority.md), [ADR-0049](0049-single-active-lease.md),
[ADR-0149](0149-a-replacement-box-numbers-above-what-the-cloud-has-seen.md),
[ADR-0027](0027-country-modules.md), [ADR-0114](0114-region-is-required-recorded-visible.md)

## The problem

ADR-0149 left one risk named: receipts a replaced box issued **offline and never synced** are unknown
to the cloud, and the replacement can issue the same numbers. Closing it needs a new range of numbers
per lease generation, which changes the printed number, so ADR-0149 deferred it to a record of its
own. This is that record.

Two further facts shape it. Where the law asks for gapless numbering, it asks for it within a
series: every jurisdiction we know of allows more than one series (a device, a register, a prefix)
as long as each is gapless and none repeats. And the chain is heading into more than one country, so
the answer has to take each country's rules without a branch in the core.

## Options considered

| | Option | Gapless | No repeat after a takeover | Cost |
|---|---|---|---|---|
| A | Keep ADR-0149's floor | Yes | **No** (offline receipts) | None |
| B | Floor plus a fixed jump (say 200) | **No** (a hole per takeover) | Almost | Small |
| C | **A new series per lease generation** | Yes, within each series | Yes | The printed number carries the series |

## Decision (proposed)

Option **C**, split three ways.

1. **The core owns the mechanism, the same everywhere.** A receipt number is a *series* and a
   *sequence*. The series is the store and its lease generation; a new generation (ADR-0049) opens a
   new series at 1 and the old one simply stops. Within a series the sequence is gapless and never
   reused. A voided bill keeps its number. The cloud checks each series for missing numbers and
   raises them. `store_server` and `device_blocks` (ADR-0025) remain the two authorities within a
   series.
2. **The country sets the rules, as part of its country module** (ADR-0027): how the pair is printed,
   when a sequence restarts (never, the calendar year, or a fiscal year such as India's from
   1 April), how long it may be, and how a voided or corrected bill is shown. A `CountryModule`
   gains one method returning that policy. A new country is a new module, and the core does not
   change.
3. **The store's configuration comes from the cloud**, as all configuration does. The store's
   required `country_code` (ADR-0114) selects the module, which supplies the default; the operator
   may change only what the module allows (for Vietnam, perhaps the two seller-chosen characters of
   an invoice symbol).

The legal invoice number is not this number and does not change: it stays with the `Fiscalization`
port and its authority-issued ranges, and ADR-0049 already gives a replacement a fresh range.

Two operational pieces come with it: a screen to **re-enter a receipt lost with a dead box**, under
its original number and time, with a manager's approval and a flag; and the cloud's gap report per
series. A lost receipt is never replaced by a new order, which would put the sale on the wrong day
and count it twice if the old data turns up.

## Before this is accepted

- **Legal and finance confirm, per country the chain opens in:** that a series per machine
  generation is allowed; the printed format; when the sequence restarts; how a void is shown. For
  Vietnam also: which provider issues the cash-register e-invoices under Decree 70/2025, and whether
  the two seller-chosen characters name the store or the machine.
- **The owner accepts the protocol change.** A settled bill's event gains the series, additively,
  with the existing number read as generation 1. `pos-proto`, `pos-core` and the country modules
  change, so each step needs an owner review.

## Consequences accepted

- The printed receipt number gains a series part. Staff and auditors see it; the format is the
  country module's, and legal signs it off first.
- Receipts issued before this change stay in series 1. Nothing is renumbered.
- A takeover can no longer repeat a number, offline or not. ADR-0149's floor remains useful within a
  series, and stops being the last line of defence.
