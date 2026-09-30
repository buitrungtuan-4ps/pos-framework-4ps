# ADR-0159 — A fee is configuration: any number of named charges, each a rate or an amount, taxed or not, by channel and by item

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-26
· Relates to [ADR-0028](0028-settlement-and-payment-invariant.md), [ADR-0033](0033-config-tree.md),
[ADR-0066](0066-cloud-catalog.md), [ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0115](0115-reason-codes-are-a-managed-list.md), [ADR-0128](0128-a-bill-splits-and-merges.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md)

## The problem

The owner decided on 2026-09-26 that every fee is configurable: a rate or an amount, subject to tax
or not, by sales channel and by item. Today there is one fee, and it is always zero.

- `billing::assemble` (`crates/pos-core/src/billing.rs`) takes a single `service_charge` amount, a
  taxable flag and one tax class. The edge passes zero (`bill_input` in `crates/pos-edge/src/app.rs`).
  No config node, cloud table, route or console screen sets it; nothing computes a percentage; nothing
  keys it on channel or item.
- The receipt prints a hardcoded English "Service charge" line, and `billing.bill.settled` carries one
  `service_charge` figure with no per-class tax lines.
- No packaging, delivery, cover or minimum charge exists. The only workaround is a menu item staff add
  by hand, which cannot be a percentage and depends on someone remembering.
- [ADR-0028](0028-settlement-and-payment-invariant.md) left service-charge taxability to configuration
  (taxable by default); roadmap D10 lists it as unspecified; roadmap-v3 B4.2 and B5.3 are open.

## Options considered

| | Option | Why not / cost |
|---|---|---|
| A | One configurable service charge | Meets the example, not "every fee" |
| B | Fees as menu items staff add | No percentages, no automatic channel rule, relies on memory |
| C | **A published list of fee rules the core applies to every bill** | A new wire node, additive event fields, and a core calculation to prove |

## Decision (proposed)

1. **A fee rule is data** in a new `fees` config node (`pos_proto::fees`), authored in the console at
   tenant, brand or store level and merged by fee id down the config tree
   ([ADR-0033](0033-config-tree.md)). A rule has:
   - `fee_id`, a stable `code` for reports, and a `display_name` with per-locale translations, as
     reason codes have ([ADR-0115](0115-reason-codes-are-a-managed-list.md));
   - `kind`: `PERCENT` (basis points of the base), `AMOUNT_PER_BILL`, or `AMOUNT_PER_UNIT` (per unit
     of each matching line — a packaging fee per box);
   - `channels`: the sales channels it applies on; empty means every channel;
   - `items`: which lines count — all, or an include or exclude list of item ids. The cloud compiles
     categories and tags into item ids per store and channel, as it compiles the menu
     ([ADR-0066](0066-cloud-catalog.md)), so the edge needs no category model;
   - `base`, for a percentage: after discounts and comps (default) or before them; net of tax
     (default) or tax-inclusive, for markets that quote a charge on the inclusive price;
   - `tax`: `NOT_TAXABLE`; `FOLLOW_LINES` — spread across the base's tax classes in proportion and
     taxed at each class's rate, the way discounts are spread today (the default, per ADR-0028); or one
     named tax class;
   - `waivable`, and `active`.
2. **The core computes; the edge only gathers inputs.** `assemble` takes the rules and the bill's
   lines (item, quantity, net, class) and returns one `FeeLine` per applied rule — its amount and its
   tax per class — folded into the tax and the total. Fees never compound: a percentage is taken of
   lines, not of other fees. Each fee rounds once to the currency's minor unit
   ([ADR-0134](0134-a-currency-says-how-many-decimals-it-has.md)), by the store's rounding mode
   (half-up unless set otherwise, ADR-0160). Property tests hold the sum laws, splits included.
3. **A bill keeps the rules it opened with.** The rules in force when a bill opens are recorded on it
   — `billing.bill.opened` gains the rule snapshot, additively — so a publish during a meal does not
   change what the pre-bill showed.
4. **Events and receipts itemise.** `billing.bill.settled` gains `fee_lines` (fee id, code, amount,
   tax) and per-class `tax_lines` (roadmap-v3 B4.1), additively. `service_charge` keeps carrying the
   sum of all fees, so today's readers — the revenue rollup, the CSV export, the ERP line — stay right
   until they move to `fee_lines`. The receipt and the pre-bill print each fee under its own name, in
   the store's print language.
5. **Waiving a fee is an act, not an edit.** A waivable fee is removed from one bill by
   `billing.fee.waived` (bill, fee, reason), under a new permission `billing.fee.waive` and a reason
   from the managed list (a new `ReasonAction`). Whether a role waives directly or with approval
   follows [ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md).
6. **Splits and merges.** Each part applies percentage rules to its own lines; an amount per bill is
   allocated across the parts in proportion to their bases, so the parts sum to what the whole would
   have been, within the per-part rounding [ADR-0128](0128-a-bill-splits-and-merges.md) already
   accepts. A merged bill applies its rules to the merged lines.
7. **Inbound orders.** A rule applies to a marketplace, QR or API order only when its `channels`
   include that order's channel. A marketplace's commission to the store is not a guest fee and is out
   of scope.

## Deferred, with a trigger

The rule shape takes these without a breaking change; each lands when a store needs it, and the owner
may pull any of them into this record during review:

- a per-guest cover charge (`AMOUNT_PER_GUEST`) — when a store charges one;
- a minimum spend topped up to a floor (`TOP_UP_TO`) — when a private room has a minimum;
- conditions on party size, bill size, time of day or day of week — when a surcharge depends on them.

## Before this is accepted

- The owner confirms the defaults: taxable (`FOLLOW_LINES`), after discounts, net of tax, not
  waivable.
- The owner confirms freezing a bill's fees when it opens (item 3) rather than when it settles.

## Consequences accepted

- **`pos-core` and `pos-proto` change** — a new calculation in `billing`, a new node, additive event
  fields — and need the owner's review; `docs/snapshots/events.txt` grows.
- **Fees by channel are only as right as the channel's prices.** The edge installs one price book, the
  dine-in one, and so prices takeaway, delivery and QR orders at dine-in prices. That is fixed first,
  as its own change, before any fee keys on channel.
- **Reports change shape.** The rollup and the export gain fees by code; until they read `fee_lines`,
  they see one combined figure, as today.
- **A fee is configuration, so a mistake in it is a pricing mistake.** The console previews a sample
  bill before publishing, and cloud-side validation refuses a percentage outside 0–100 % and an
  `items` list that names nothing on the store's menu.
