# Product specification

**Status** Accepted · **Owner** @maintainers-domain · **Last reviewed** 2026-08-18

What the product does. Implementation is in [architecture.md](architecture.md); naming rules are in [naming-and-api.md](naming-and-api.md).

Everything here runs **offline** at the store unless explicitly marked as a cloud feature.

---

## 1. Roles and channels

Sales channels: dine-in, takeaway, delivery marketplaces, external channels via API, and guest QR ordering.
Staff roles ship as editable templates: server, cashier, shift lead, store manager, brand manager, tenant admin, accountant (read-only), auditor (audit log only).

## 2. Tables, tabs, and seats

- Floor plan per area. Table states: `FREE → OCCUPIED → AWAITING_PAYMENT → NEEDS_CLEANING → FREE`.
- **A table has exactly one open order at a time.** This is a state-machine invariant, not a UI convention: a second open order on an occupied table is unrepresentable.
- **Transfer** moves an order to another table; **merge** combines two orders while each line keeps its origin so the kitchen is not confused.
- **Tabs** (capability flag `tabs_enabled`): an open order identified by a guest name rather than a table — for bars and counters.
- **Seats** (capability flag `seats_enabled`): each line may carry a seat number, which enables splitting a bill by seat.

## 3. Ordering

- Add items from the menu; each line carries quantity, modifiers, and a free-text note. **The note's text stays at the store and never enters the event log** — it is exactly where "for Mr Nguyễn, severe peanut allergy" gets typed, and the log is immutable, so anything personal in it could never be erased. Events carry only whether a note exists; the kitchen reads the note from the local order record. No report, reconciliation or ERP posting has any use for it.
- **Modifier groups** may be required or optional and may nest. A dedicated `SPLIT_ITEM` modifier supports half-and-half products: one line, two halves, configurable pricing (**default: the higher-priced half**), and a bill of materials computed per fraction.
- **Courses** group lines (starter, main, dessert). Fire by course or fire everything.
- **Fire** sends lines to the kitchen. Before firing, lines are freely editable. After firing, cancelling requires a permission, a reason, and prints a **VOID ticket** at the correct station.
- **Hold** keeps a line unfired. **86** marks an item unavailable: it greys out on every device instantly and tells delivery marketplaces the item is temporarily out.
- **Open item**: an ad-hoc item with a typed name and price. Separate permission, always audited.
- Every order action is an **append command**, so two devices working on the same table merge cleanly.

## 4. Kitchen

- Lines route to stations by configured rules (one station per item in v1).
- **Kitchen display per station**: cards grouped by order and course, an age timer per card, colour change past a configured threshold. **Bump** completes; **recall** brings back the last 60 seconds.
- **Expo screen** (optional) aggregates bumped items by table for runners.
- Printing runs a **queue with retry**. A failed station printer falls back to a configured backup printer or raises a red badge on that station's display. Screens are the fallback for paper, and paper is the fallback for screens.

## 5. Billing and payment

- Bill = subtotal − discounts + service charge + tax, rounded by configured rules. Service charge is applied **after** discounts and **before** tax.
- **Tax is per item class, not per store.** Every menu item carries a `tax_class` (food, drink, alcohol, …) and the rate is resolved from a table in the store's locale pack **keyed by sales channel** — the same item can be taxed differently takeaway and dine-in (Japan charges 8% and 10% respectively). The config key is `store.tax.tax_class_rates`. A flat store-level rate is a *special case* of this table, not a different model: Vietnam v1 populates one class at one rate. Both `tax_class` and the channel dimension are in the schema from day one because retrofitting them is a migration across every order line ever written, and the rate in force is captured in the line snapshot (§14.2) rather than looked up later.
- **Split** by item, evenly into N, or by seat. **Merge** before payment.
- **Discount** by percentage or amount, per line or per bill, with a mandatory reason. Above the role's ceiling it requires a manager PIN entered on the same device.
- **Comp** (giveaway) is distinct from discount and from void: a comp still consumes inventory and is recorded as cost. Accounting and fraud analysis treat the three differently.
- **Payment methods** — cash, card, QR/wallet, voucher, gift card (reserved), other — and **several may be combined on one bill**. Card payments always have an *unknown result* branch, which parks the bill for reconciliation rather than guessing.
- **Tips**: `tip_amount` is stored separately from the sale amount. Tips may be adjusted after the card is captured (`billing.tip.adjusted`), cash tips are declared at shift close, and a distribution report exists. The UI is enabled per locale; the data model always carries it.
- **Receipt numbers increase without gaps per store.** The counter is incremented inside the same SQLite transaction as the bill, so offline operation never skips a number.
- **Void and refund** after settlement require a manager, a reason, and print a void slip. Refunds happen **only at the store that issued the bill**.

## 6. Shifts and cash

Open a shift with a starting float; every transaction attaches to it. **One open shift per cashier device at a time** — the shift a transaction belongs to is never ambiguous. Paid-in and paid-out entries carry reasons. **Closing is blind**: the cashier enters the counted amount *before* the system reveals the expected amount, and only then is the variance shown. Opening the drawer outside a sale requires a permission and is logged.

## 7. Pricing and promotions

One model covers happy hours, item and category discounts, combos, vouchers, and manual reductions:

```
Campaign = scope (tenant/brand/store) + schedule (hours, weekdays)
         + conditions (items, categories, minimum bill, sales channel, customer group)
         + action (percentage, amount, combo price, free item)
         + stacking rules (exclusion groups, priority) + quota + optional voucher codes
```

**Evaluation order is deterministic:** item-level → combo → bill-level → voucher → manual. Every applied campaign appears as its own line on the bill.

**Timing:** item and combo rules are evaluated **when the line is added**; bill-level rules and vouchers **when payment begins**. A guest who ordered at 16:59 keeps the happy-hour price even if they pay at 17:30.

**Offline rule of the engine:** *rules run offline, uniqueness runs online.* Happy hours, combos, and manual discounts work with no connection. **Voucher redemption is an atomic check-and-mark against the cloud**, which makes double redemption impossible. With no connection the voucher button is greyed out; everything else still sells.

**Channel price lists** are separate from promotions: dine-in, takeaway, and each marketplace may have different base prices (marketplace prices usually absorb commission).

## 8. Inventory

- **Ingredients** with units (g, ml, piece) and their own categories.
- **Bill of materials per item *and* per modifier** — a large pizza is the base recipe plus 50 g of dough.
- **Stock is deducted when the line is fired**, because that is when the kitchen consumes it. Cancelling a fired line records **waste**, it does not return stock (configurable).
- **Available-to-make**, recalculated in microseconds and pushed to every screen in under 50 ms:

```
available(item) = floor( min over ingredients ( stock[i] ÷ recipe[item][i] ) )
```

Shared ingredients propagate correctly: if A needs D and E, and B needs C and D, cooking A lowers B's availability because D was consumed. Hitting the threshold triggers **auto-86** plus a notification to marketplaces. The cloud builds the same projection from consumption events (1–3 s behind) for brand-level views.

- Ledger entries: consumption (automatic), receipt, adjustment, waste, **stocktake** (records `counted_qty` and `count_time`; the delta is computed against the projection *at the moment of counting*, so sales during the count do not corrupt it).
- Two modes per tenant: **ERP-led** (the ERP owns master data and purchasing, the POS emits consumption) and **standalone** (the framework provides manual receipts, stocktakes, waste, and thresholds).
- Availability numbers are theoretical; spillage makes them drift. Stocktaking is what pulls them back to reality. The two belong together.

## 9. Permissions (RBAC)

Permissions are a fixed catalogue owned by the framework. **Roles are data**: sets of permissions plus parameters, edited in the cloud, synced to stores, evaluated at the edge so offline behaves identically. Any permission may be flagged "requires PIN on the spot".

Groups: sales · billing · cash and shifts · menu and inventory · store administration · cloud-only administration. Role parameters include discount ceilings (percentage and amount) and price-override ceilings.

**Adding a permission must not be manual work.** Each permission is a declaration in `pos-core` carrying id (`domain.resource.action`), group, description, risk level, default roles, and PIN flag. **Deny by default.** Adding one makes the compiler force template updates, makes the dashboard show it automatically, tags the audit log, and regenerates the documented matrix. Removing a permission is forbidden — deprecate only — and CI keeps a snapshot so any change is a visible diff. Enforcement goes through a single `require(permission)` function; ad-hoc role string checks are banned.

## 10. Store profiles (capability flags)

A profile is a set of flags in the configuration tree — `tables_enabled`, `tabs_enabled`, `seats_enabled`, `kds_enabled`, `courses_enabled`, `pay_first_enabled`, `barcode_enabled`, `queue_number_enabled`, `tips_enabled`, `qr_ordering_enabled` — not three separate applications.

| | Full-service | Cafe / counter | Retail *(data model day 1, UI phase 2)* |
|---|---|---|---|
| Start screen | Floor plan | Order at counter | Barcode field |
| Payment | After service, splits | **Before service**, queue number | At the counter |
| Kitchen | Multi-station KDS + courses | Single station or printer only | None |
| Inventory | Recipe-based | Simple recipes | 1:1 by SKU |

Items carry `sku`, `barcode`, and variant fields from day one so enabling retail later needs no migration. Flags are read through a single `CapabilityContext`; scattering `if flag` through the code is banned. Validity rules between flags (for example, `pay_first_enabled` implies tables are off) are validated in the cloud before a config version is published.

## 11. Fraud controls

1. **Blind shift close** (§6) — removes "count until it matches".
2. **Mandatory reasons** from a cloud-managed list for: voiding fired lines, discounts, comps, refunds, bill voids, opening the drawer outside a sale.
3. **Per-employee analytics** on the dashboard: void, discount, refund, drawer-open, and reprint rates against peers, with automatic outlier flags.
4. **Reprints** are marked COPY, counted, and permissioned. **Price override** is a separate permission with its own ceiling.
5. Immutable audit log plus NTP-synchronised clocks, so records line up with store camera footage where it exists.
6. **No training mode.** "Sell without recording" is a classic fraud vector. Training happens in a demo store with a sample menu.

## 12. Internationalisation

**Two tiers of strings.** *Framework UI strings* (buttons, labels, errors) are embedded in the binary, shipped as language packs with releases, and may be overridden per tenant. *Tenant content* (item names, categories, modifiers, campaign names, receipt templates) lives in the configuration tree as `{"en": required, "vi": ..., "ja": ...}`, syncs like any config, and works offline.

**English is always present and is the fallback**, so no screen ever shows a blank or a raw key; missing translations count toward a completion percentage shown to admins.

**Resolution order:** employee → device → store → brand → tenant → `en`. **Guest language is separate from staff language** — staff may work in Vietnamese while receipts print in Japanese. Kitchen tickets follow the *station's* language.

**Standards:** keys are `domain.screen.element`, append-only; messages use **ICU MessageFormat** for interpolation and plurals; dates, numbers, and currency follow the store's locale pack. CI blocks hardcoded user-visible strings.

**Thermal printer caveat:** most ESC/POS printers lack full Unicode fonts, so Vietnamese diacritics and CJK print as garbage. Any line containing characters outside the printer's code page is **rendered as a bitmap** before printing — a few milliseconds slower, correct on every model.

Cloud screen: a translation grid (English ↔ target), a "missing only" filter, completion percentage, and CSV import/export.

## 13. QR ordering (cloud module)

Guests scan a QR code on the table, open a web app served by the **cloud**, pick a language, browse the menu at `QR` channel prices, and submit. The cloud validates and publishes the order over NATS; the edge receives it through the same `OrderIn` port used by marketplaces. If the table already has an open order, the items append to it, tagged `channel = QR`.

**Deliberate boundaries.** Payment in v1 happens **at the counter** — online payment gateways are a future adapter group. QR ordering is a **cloud feature and is not offline-first**: if the store or the cloud is unreachable, the page tells the guest to call a staff member. Staff are always the fallback, and no end-customer SLA is promised.

**Abuse controls for a static printed QR code** (which can be photographed and used from outside): **staff confirmation before firing is on by default**, plus per-table rate limits, orders accepted only during opening hours, and only while the store is online. An optional guest phone number is PII and follows §15.

## 14. Data-correctness laws

These are invariants, not preferences. Property tests are written against them.

1. **Business date.** Each store has a configurable day-cutoff hour (default 04:00 local; a daytime shop may set 00:00). Every event is stamped with `business_date`; rollups, reports, and shift closes use it, so a bill rung at 01:30 belongs to the previous evening. The country module uses the **calendar date** for legal invoices. Two concepts, two fields, never mixed.
2. **Line snapshots.** A line captures price, tax class and rate, display name, and promotion outcome **at the moment it is added**. It never references the live menu. Changing or deleting a menu item does not alter open orders.
3. **Split rounding.** The parts of a split must sum **exactly** to the original total; the rounding remainder goes to the last part, and bill-level discounts are allocated proportionally. CI asserts `sum(splits) == original_total`.
4. **Exclusive settlement.** `bill:settle` is a one-time state transition. A second attempt returns `FAILED_PRECONDITION`. The UI takes a soft lock when a payment screen opens and tells other devices who is paying.

## 15. Tenant lifecycle and personal data

The framework provides **mechanism**; the operator sets **policy**.

Three platform operations: **Suspend** (service off, data retained, stores read-only), **Export** (the tenant's full data as a downloadable package), **Delete** (anonymise personal data and remove operational data while retaining financial records).

The framework hardcodes **no retention period** and makes no legal determination — retention is configuration, defaulting per country locale pack, because the operator is the data controller. What the framework does guarantee technically: **after anonymisation, financial figures still reconcile**.

**Mandatory design consequence:** the event log is immutable, so **PII is never embedded in event payloads**. Personal data lives in a separate record keyed by `subject_id`; events carry only the id. Anonymising is deleting one record — the log is never rewritten. (Equivalent alternative: encrypt per subject and destroy the key.) Without this decision, "delete one customer's data" later means rewriting all history and every backup.

## 16. Other operational rules

- **Employee PINs**: minimum length configurable, unique within a store, locked for 5 minutes after 5 failures (enforced offline too), every failure audited. Device activation codes and setup tokens are rate-limited and expire after repeated failures.
- **Queue numbers reset daily**, on the store's business date. They are a customer-facing call number for counter service and are a different counter from the store-lifetime, gapless receipt number (§5) — the two must never share an implementation.
- **Bulk menu import** (CSV/Excel with preview, per-row errors, and column mapping) at brand creation — required for chains migrating from another POS.
- **Employees belong to a tenant; roles are granted per store.** Working at two stores means two grants, one `employee_id`, one PIN.
- **Device screen lock** after N idle minutes, unlocked by PIN.
- **Buyer details** on a bill (`buyer_name`, `buyer_tax_code`, `buyer_email`) are optional PII feeding the country module's corporate-invoice flow.
- **Marketplace order containing a just-86'd item**: configurable per vendor — reject the whole order, or accept without the item and notify.
- **Menu scheduling (dayparts)**: items and categories may be restricted to time windows and weekdays — this controls *whether an item is sold at all*, unlike happy-hour pricing which only changes the price. Configured per brand, overridable per store.
- **Online order throttling**: a configurable ceiling on marketplace and QR orders accepted per 15-minute window, with the resulting prep time published back to the vendor. Protects the kitchen during peaks instead of silently accumulating late orders.
- **Customer-facing display (CFD)**: an optional device role showing the running order and total to the guest at the counter. It is a device type in the existing device model, paired and revoked like any other.
- **Time clock (light)**: employees clock in and out with their existing PIN; the system reports hours worked per employee and per shift. Payroll is explicitly out of scope — this exists so labour hours can be compared with sales in reporting.
- **Receipt customisation**: logo, footer text, and a promotional line, configured per brand and overridable per store; rendered as bitmap where the printer lacks the character set (§12).

## 17. Reporting

At the store, one **Today** screen: revenue, bill count, average bill, revenue by payment method and by hour, top items, total discounts and voids with the underlying list.

In the cloud: day, month, and **custom date ranges**, comparisons across stores and brands, filtering by customer group, and **CSV export** on every report.

**Customer groups** (staff, VIP, members) exist for exactly two purposes: as a condition in the pricing engine, and as a reporting dimension. They are explicitly **not** a CRM — no per-customer purchase history, no points, no profiles.

**PDF menu generation** (cloud): pick brand, store, and language, and export a printable menu or a **view-only** QR page. Ordering from that page is QR ordering (§13); the PDF path stays view-only.

## 18. Event catalogue

Names follow `domain.resource.action` ([naming-and-api.md](naming-and-api.md) §5), with a shared envelope carrying `event_id`, `event_type`, `event_time`, `business_date`, `schema_version`, and context ids.

```
sales.order.opened · sales.order_line.added / .updated / .voided / .fired
sales.table.transferred / .merged · sales.qr_session.started
sales.order.submitted_by_guest / .confirmed_by_staff / .rejected_by_staff
kitchen.ticket.bumped / .recalled
billing.bill.split / .merged / .settled / .voided
billing.discount.applied · billing.comp.applied · billing.payment.captured
billing.tip.adjusted · billing.refund.issued
cash.shift.opened / .closed · cash.drawer.opened / .paid_in / .paid_out
inventory.item.sold_out / .restored
inventory.stock.consumed / .adjusted / .counted
promotion.voucher.redeemed
delivery.shipment.created / .status_changed
config.version.published · device.activation.completed · fleet.update.rolled_out
```

**Eleven further types complete the set.** Each exists because a rule stated elsewhere in
this document had no event able to carry it, and the asymmetry of the naming standard is
what makes declaring them early correct: adding an event type is additive and free, while
removing one is forbidden. So the cost of declaring one now is near zero, and the cost of
discovering a missing one after a thousand stores hold offline data is a protocol version
bump.

```
sales.order.closed · sales.order_line.held
sales.table.opened / .closed
billing.bill.opened
cash.shift.counted
inventory.stock.received / .wasted
promotion.voucher.reserved
security.permission.overridden
config.version.activated
```

| Type | The rule that needs it |
|---|---|
| `security.permission.overridden` | §11.4 makes a manager-PIN override above a ceiling a fraud control. Nothing carried it, so the control had **no auditable record at all** |
| `cash.shift.counted` | §6 requires the close to be **blind** — the count entered before the expected amount is revealed. Folded into `shift.closed`, the blindness is unverifiable afterwards, which defeats the control |
| `inventory.stock.wasted` | §8: cancelling a fired line records waste and does *not* return stock. The ledger has waste entries; the catalogue had no waste event |
| `inventory.stock.received` | §8 names receipt as one of the five ledger entry kinds |
| `promotion.voucher.reserved` | §7 makes redemption an atomic check-and-mark, so reserve and redeem are distinct states. With only redemption, a settlement that then failed would burn the voucher |
| `billing.bill.opened` | Split, merged, settled and voided all presuppose a bill that nothing created |
| `sales.order.closed`, `sales.order_line.held` | §3 requires hold; the order lifecycle had no terminal event distinct from settlement |
| `sales.table.opened`, `sales.table.closed` | §2's table cycles through five states; only two transitions had events |
| `config.version.activated` | Published is not the same as running. The fleet view needs to know which store is actually on which version |

This catalogue is the single source for chain reporting, reconciliation, ERP posting, and webhooks. It is rendered to `docs/snapshots/events.txt` from the code, and CI refuses any removal from that file.

## 19. Scope

**Excluded on purpose**, each for a stated reason: reservations and waitlists (demand unproven) · buffet and time-based billing · CRM, loyalty, and marketing (separate domain; the public API lets tenants connect an existing system) · multi-currency within one bill · self-service kiosks and online storefronts (`POST /v1/orders` already lets anyone build them) · gift cards (needs an online balance ledger like vouchers; the payment-method slot is reserved) · payroll and full purchase orders (ERP territory) · **training mode** (§11.6) · payment processing — we integrate terminals and stay acquirer-neutral, which costs us transaction revenue and buys the operator freedom from lock-in.
