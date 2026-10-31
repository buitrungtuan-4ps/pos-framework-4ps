# ADR-0184 — A settled bill is voided whole, not refunded

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-10-31 · Amends
[ADR-0028](0028-settlement-and-payment-invariant.md) (the consequence on refunds) · Relates to
[ADR-0025](0025-receipt-number-authority.md), [ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0156](0156-a-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0167](0167-a-till-has-its-own-cash-drawer.md)

## Context

A store charges a guest wrongly: the wrong table's bill, a dish that never came, card instead of
cash. Today the till has no answer. `SETTLED` is terminal (ADR-0028), the bill state machine voids
only an `OPEN` bill (`crates/pos-core/src/machines.rs`), and a settled bill offers only a reprint.
Pilot runs 1–3 recorded this as F3.

The schema anticipated two answers and built neither. `billing.bill.voided` is documented as *"a
settled bill was voided, which requires a manager and a reason"*, and `billing.refund.issued` carries
an amount. `pos-spec.md` §1 asks for both: *"Void and refund after settlement require a manager, a
reason, and print a void slip."* ADR-0028 says a refund is a new signed movement, never a mutation.

The owner decided on 2026-10-31: **no refunds; a bill that was paid is voided.**

## Options considered

1. **Refund an amount** (`billing.refund.issued`). Partial money back against a settled bill.
   Rejected by the owner: a partial refund is a second, free-form way to take money out of the
   drawer, it needs its own tax and invoice arithmetic per amount, and a wrong charge is fixed just as
   well by voiding and ringing the bill again.
2. **Void the settled bill whole** (chosen). One act, one state the machine already has, one event
   the schema already publishes. Re-ringing a corrected bill is an ordinary new bill.
3. **Do nothing in the till; fix wrong charges in the cloud.** Leaves cash in the drawer that the
   books say was never taken, and the shift close wrong.

## Decision

1. **`SETTLED → VOIDED` becomes a legal transition.** `VOIDED` stays terminal. No enum value is
   added, so `pos-proto`'s `BillState` and `PROTOCOL_VERSION` do not change.
2. **The whole bill, never part of it.** There is no amount to enter. To correct a bill, void it and
   ring a new one; the new bill takes the next receipt number.
3. **The event is the one already published.** `billing.bill.voided` (`bill_id`, `reason_code_id`)
   is emitted in the same transaction as the state change (`TxContext`), and the envelope names the
   person and the device as it does for every event. No new event, no PII.
4. **A permission of its own, and a reason.** A new permission `billing.bill.void_settled`, PIN
   required, granted by default to Manager and Owner (ADR-0158): forgiving money already taken is
   not the same act as voiding a bill nobody paid, and a store may want to grant them differently.
   The reason comes from the `VOID_BILL` list (ADR-0115). The till prints a void slip naming the
   receipt number, the amount and the approver.
5. **The receipt number stays used.** The voided receipt keeps its number, so the gapless series
   (ADR-0025) counts every sale that was taken, voided or not. A reprint of it is marked VOID.
6. **Money follows the payments that were taken.** Each payment's `applied_to_bill` is reversed by
   its method:
   - **Cash** leaves the drawer of the till where the void is done (ADR-0167), so that drawer's
     expected cash at close falls by the amount, and the close card and X/Z reports list it as
     "voided after payment".
   - **Card and other methods** are recorded as reversed; the money goes back through the card
     terminal or provider, which the till does not drive today, so the void slip says so.
   - **Tips** are not touched: they are a separate ledger (ADR-0028), adjusted with
     `billing.tip.adjusted` if needed.
7. **Only within a window, which is configuration.** A new store-wide setting
   `billing.settled_void_window` (ADR-0160), `SETTLED_VOID_WINDOW_BUSINESS_DATE` by default (the
   bill's own business date) or `SETTLED_VOID_WINDOW_SHIFT` (only while the shift that took the money
   is open). Outside the window the till refuses and says why. Keeping the void on the sale's
   business date means no rollup has to restate a closed day.
8. **Not when a legal invoice was issued for the bill.** Cancelling or replacing an e-invoice is
   undecided (ADR-0156, "Not decided here"). Until a record decides it, the till refuses to void a
   bill with an issued e-invoice and says the invoice must be cancelled first; the country module,
   not the core, says whether one was issued.
9. **Reports.** Sales, tax and tender totals for the business date exclude a voided bill; the void
   count and amount appear per employee beside the other control rates (`pos-spec.md` §11 item 3).

## Consequences accepted

- `billing.refund.issued` stays published, because a published event is never removed, and nothing
  emits it. A later record can use it if a refund is ever wanted.
- One permission and one setting are added: the permission and settings snapshots gain lines,
  nothing is removed. The core's bill machine, the edge's settle path, the shift's expected cash and
  the cloud's rollups change in code; each has tests that a void after payment balances.
- A store that needs to correct a bill from an earlier day cannot do it at the till. That is
  deliberate: it is an accounting correction, and it belongs in the cloud with its own record.
- A card void is only recorded. Until the till drives a card terminal, the cashier must also reverse
  the charge on the terminal, and the void slip is the reminder.
