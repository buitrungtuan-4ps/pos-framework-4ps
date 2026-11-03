# ADR-0186 — A drawer is counted by denomination, and a counted drawer takes no more cash

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0105](0105-a-country-pack-is-values.md) (the coin list it ruled out) and
[ADR-0167](0167-a-till-has-its-own-cash-drawer.md) (what a counted drawer takes) · Relates to
[ADR-0028](0028-settlement-and-payment-invariant.md), [ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md)

Accepted by the owner on 2026-11-03 ("ok hết, làm luôn"), with the condition that the counting list
is not fixed per country but each store can change it.

## Context

A till counts its float and its close as one typed number (`Shift.tsx`, `cash.shift.opened.opening_float`,
`cash.shift.counted.counted_amount`). Cashiers count notes by kind, so they add up on paper and type
the sum. A wrong sum is recorded as a variance, and nobody can see afterwards which pile was wrong.
The country pack's `cash_denominations` lists only the notes a guest hands over (VN 10k to 500k), so
it cannot drive a count. ADR-0105 kept it that way on purpose and ruled out a coin list "until
something reads it". A drawer count now reads it.

Two related gaps surfaced in the same UX review:

- **A counted drawer still takes cash.** A cash settle counts toward a shift while it is trading or
  counted (`pos-edge-app` `shifts/drawers.rs`), but paid in and paid out are refused once it is
  counted. A sale rung between **Enter count** and **Close** raises the expected amount after the
  count is fixed, so the close reads short.
- **A safe drop is not its own act.** Taking surplus notes to the safe is recorded as a paid out with
  a reason, if the store's list has one, so reports count it with expenses.

## Options considered

1. **A counter in the till only, adding up to today's single figure.** It helps the cashier, but the
   breakdown is lost, and the slip and the console cannot show which pile was short.
2. **Record the breakdown on the count events, and give the pack and the store a counting list**
   (chosen).
3. **A separate count document per drawer.** This adds a second record of the same act, against
   ADR-0028's one-movement-one-event rule.

## Decision

1. **A counting list, separate from the quick-cash list.**
   - The country pack gains `cash_count_denominations`: every note and coin in use, ascending, in
     minor units. For example: VN 1k, 2k, 5k, 10k … 500k; JP ¥1 … ¥10,000; IN ₹1 … ₹500.
   - `cash_denominations` keeps its job (the Pay screen's quick keys) and its meaning.
   - The `locale` node gains the same optional field, so a store or a chain overrides the pack, as it
     already does for rounding (ADR-0105, "the country states the fact, the store overrides it").
     The console offers it beside the quick-cash list.
   - An empty list means no counter: the till keeps the single amount field.
2. **The count events carry the breakdown, additively.**
   - `cash.shift.opened` and `cash.shift.counted` gain an optional `denomination_counts`: a list of
     `{denomination, quantity}` plus `uncounted_amount` for anything the list does not name.
   - The edge refuses a count whose lines do not add up to the amount, with `422 COUNT_DOES_NOT_ADD_UP`.
   - The amount fields keep their meaning, and an event without the list is read as today.
   - The breakdown is money and quantities only: no person, no PII.
   - Both events' `schema_version` stays 1, because the field is optional. `PROTOCOL_VERSION` is
     checked as `pos-proto` requires.
3. **The till's counter.** One row per denomination: a quantity, with − and + keys and the keypad,
   and the row's value; then an "Other / uncounted" amount and the running total. The total is what
   the act records. The printed shift report and the close card list the rows.
4. **A counted drawer takes no more cash.**
   - Once a shift is counted, a cash settle, a cash void and a cash split part into that drawer are
     refused with `409 DRAWER_COUNTED`. The till says "This drawer is being closed. Take this payment
     at another till, or by card."
   - Other tenders still settle.
   - The count is then a true snapshot, and expected cash cannot move after it.
5. **A safe drop is its own act.**
   - A new event, `cash.drawer.dropped` (`amount`, `terminal_device_id`, optional `reason_code_id`,
     optional `denomination_counts`).
   - It is permitted by the existing `cash.movement.record`.
   - It lowers expected cash like a paid out, but reports, the close card and the shift report show
     it as "Dropped to safe", apart from expenses.
   - The till offers it on the Shift screen beside paid in and paid out, with the counter.
   - A new event is additive; `PROTOCOL_VERSION` is checked as `pos-proto` requires.

## Consequences accepted

- **Additions to `pos-proto`:** an optional field on two events, one new event, and one optional
  `locale` field. The events and settings snapshots gain lines and lose none.
- **Packs:** each pack states a counting list. `countries/zz` documents that an empty list is a valid
  answer.
- **Cash at the end of a close:** a cashier who counts first and then takes a last cash payment is
  refused and must use another till or a card. This is deliberate: that cash belongs to no count.
- **Older edges:** an older edge ignores the new fields, and an older till sends none. Nothing
  breaks; the breakdown is simply absent.
