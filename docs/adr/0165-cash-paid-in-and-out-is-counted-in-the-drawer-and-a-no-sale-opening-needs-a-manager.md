# ADR-0165 — Cash paid in and out is counted in the drawer, and a no-sale opening needs a manager

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-30
· Relates to [ADR-0100](0100-receipt-and-ticket-printing.md),
[ADR-0103](0103-directly-attached-printers.md),
[ADR-0112](0112-print-agents.md),
[ADR-0115](0115-reason-codes-are-a-managed-list.md)

## The problem

`docs/pos-spec.md` §6 says "paid-in and paid-out entries carry reasons" and "opening the drawer
outside a sale requires a permission and is logged". The parts exist and nothing uses them:

- The events `cash.drawer.paid_in`, `cash.drawer.paid_out` and `cash.drawer.opened`, the
  permissions `cash.movement.record` (cashier and up, no PIN) and `cash.drawer.open_no_sale`
  (supervisor and up, PIN), and the reason actions `CASH_PAID_IN`, `CASH_PAID_OUT` and
  `DRAWER_OPEN` are all defined. No command, route or screen produces any of them.
- The expected drawer at close is the float plus the cash taken on bills. Cash taken out to pay a
  supplier, or change brought in from the bank, therefore shows up as a variance the cashier did
  not cause, and the variance is the control (§11.1).
- No drawer ever opens, not even on a cash sale. The published device does not say which printer
  has a drawer wired to it, so `assumed_capabilities` reports `kicks_drawer: false`, and
  ADR-0103 left the fix to a console field.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Record the movements in the log, and leave the expected drawer as it is | Every movement is still a variance at close |
| B | Ring a movement up as a sale of a "cash" item | A movement is not a sale: it would be taxed, receipted and reported as revenue |
| C | **The three events from the shift screen, counted in the expected drawer, and a drawer kick where the console says a drawer is wired** | Three routes, one screen section, one additive device field and one console checkbox |

For the kick alone, sending it to every USB receipt printer needs no field, and ADR-0103 has
already rejected it as "a behaviour nobody asked for".

## Decision (proposed)

Option **C**. The owner approved these five points on 2026-09-30.

1. **Paid in and paid out.** The shift screen offers **Paid in** and **Paid out**: an amount on the
   keypad, then a reason from the store's list for that act (ADR-0115). The permission is
   `cash.movement.record`, with no PIN. The amount must be above zero. A movement is recorded only
   while the shift is open, before its blind count, because a movement after the count would change
   the expectation the count is checked against. Each one is `cash.drawer.paid_in` or
   `cash.drawer.paid_out` on the shift's envelope. Routes: `POST /api/shifts/{id}/paid-in` and
   `POST /api/shifts/{id}/paid-out`.
2. **The expected drawer** is the opening float, plus the cash taken on bills, plus paid in, minus
   paid out. The close records it in `cash.shift.closed` as it does today, and the shift report
   prints paid in and paid out as their own lines, so the paper adds up. A paid out larger than the
   drawer should hold is not refused: the refusal would tell the cashier roughly what the drawer
   holds, and the close is blind. The variance shows it.
3. **A drawer opened outside a sale.** The shift screen offers **Open drawer**. It needs
   `cash.drawer.open_no_sale`: a manager types their badge code and PIN in place, as for a void,
   and picks a reason (`DRAWER_OPEN`). It writes `cash.drawer.opened` with `standalone: true` and
   the reason, and the `security.permission.overridden` every verified step-up writes. It is allowed
   with or without an open shift, because it moves no cash. Route: `POST /api/drawer/open`.
4. **The drawer kick.** The console marks a printer **Cash drawer attached**. The mark is published
   as a new field on the device, `cash_drawer` (default false), which closes what ADR-0103 left
   open. A drawer opens only through such a printer, only when it serves the bill rather than a
   station, only over USB (`docs/architecture.md` §5), and only when the edge writes its bytes
   itself (a print agent carries print jobs, not a kick, ADR-0112). It opens on a cash payment, a
   paid in, a paid out and a no-sale opening. Only the no-sale opening writes `cash.drawer.opened`,
   because the other three are already in the log. When no drawer answers, the act still stands,
   and the till says to open the drawer with its key. Each of these responses says what happened:
   `OPENED`, `NO_DRAWER` or `DRAWER_UNAVAILABLE`.
5. **Not in this round.** A printed slip for each movement, since the shift report prints the
   totals, and the X/Z reports, which need decisions of their own (`docs/roadmap.md` D10).

## Consequences accepted

- **`pos-proto` gains a field** on `PublishedDevice`, so this needs the owner's review under
  AGENTS.md §6. There is no new event and no new permission, and `PROTOCOL_VERSION` is unchanged.
  An edge that does not know the field ignores it and opens no drawer, as every edge does today.
- **A shift that records a movement expects a different drawer.** A shift without one closes
  exactly as before. The cloud's daily cash rollup already folds paid in and paid out, and takes the
  expected figure from `cash.shift.closed`, so it agrees without a change.
- **The cloud gains a column**, `device_proposals.cash_drawer`, in an additive migration, and the
  console gains the checkbox on a USB printer.
- **A drawer behind a print agent stays shut** until the agent protocol carries a kick. The till
  says to use the key.
- **Whether a drawer springs is a desk test** (gate P8). CI proves the kick goes to the marked
  printer and to no other.
- **Until ADR-0158's per-person enforcement lands**, the store-wide permission set decides who may
  record a movement. The no-sale approver is already checked person by person.
- Two refusal tokens: `SHIFT_NOT_OPEN` and `CASH_REASON_NOT_VALID`.
