# ADR-0164 — A receipt is reprinted as a marked copy, and every reprint is counted

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-30
· Relates to [ADR-0025](0025-receipt-number-authority.md),
[ADR-0100](0100-receipt-and-ticket-printing.md),
[ADR-0129](0129-a-receipt-itemises-what-was-sold.md),
[ADR-0162](0162-a-bill-is-taxed-at-the-rates-of-one-stated-moment.md)

## The problem

`docs/pos-spec.md` §11 item 4 says "reprints are marked COPY, counted, and permissioned", and item 3
puts each employee's reprint rate on the dashboard. None of it exists:

- The permission `billing.receipt.reprint` is in the catalogue (cashier and up, no PIN), and no
  command checks it.
- No route prints a settled bill's receipt again, and the till has no list of settled bills to
  choose one from. A guest who asks for a copy for their expenses cannot have one.
- No event records a reprint, so nothing could count one if it happened.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Reprint as a print job only, with no event | Not counted, so §11 item 3 has no data, and the log cannot tell a copy from nothing |
| B | Reprint under a new receipt number | One sale with two numbers in a gapless series (ADR-0025) |
| C | **A marked copy under the same number, and an event for each reprint** | One additive event, two routes, one screen |

## Decision

Option **C**. The owner approved these five points on 2026-09-30.

1. **Every reprint is an event.** `billing.receipt.reprinted` carries `bill_id`, `receipt_number`
   and `copy_number` (1 for the first copy), additively. The envelope already names the employee
   and the device, so a per-employee reprint rate is a count. No PII: a buyer's details stay in the
   subject store, as they do for the original ([ADR-0107](0107-the-buyer-is-a-subject.md)).
2. **The permission is the catalogue's.** `billing.receipt.reprint`, no PIN. Every copy is
   counted, and flagging the outliers is the dashboard's job (§11 item 3).
3. **Today's bills.** A copy can be printed for a bill this store settled in the current business
   day. Older bills are reprinted from the console, later.
4. **Where.** The Today screen's **Recent bills** lists today's settled bills, newest first, at
   most 200, each with its receipt number, table or queue number, total and time. A tap on one,
   then **Reprint**. The pay screen offers **Print again** right after a settle, which is the same
   act and is counted the same way. Routes: `GET /api/bills/settled` and
   `POST /api/bills/{id}/receipt/reprint`. A bill that has not settled has no receipt to copy
   (`409 NOT_SETTLED`); its check is what the pre-bill prints.
5. **What prints.** The original document, from the line snapshots (ADR-0129) and the totals
   `billing.bill.settled` recorded, under the same receipt number. Under the header it says
   **COPY** (**BẢN SAO** in Vietnamese) and "Reprint *n* · *time*". A copy never prints a figure
   the settle did not record. The per-rate tax lines are computed again from the rate table in
   force, because the settle does not record them yet. If they do not add up to the recorded tax,
   the copy prints the recorded tax total alone. ADR-0162 item 3 records the rates on the settled
   bill, and from then on a copy prints them exactly.

## Consequences accepted

- **`pos-proto` and the event schema gain an event**, so this needs the owner's review under
  AGENTS.md §6, and `docs/snapshots/events.txt` grows.
  - `PROTOCOL_VERSION` is unchanged. A receiver that does not know the type stores and forwards it
    (`EventTypeRef`).
- **A reprint is counted when it is asked for**, not when paper comes out. A print job that is
  retried is not a second copy.
- **Until ADR-0158's per-person enforcement lands**, the store-wide permission set decides who may
  reprint, as it does for every permission today.
- **The dashboard's reprint rate** reads the new event in a later change. Until then the events
  are in the log, and nothing reports them.
