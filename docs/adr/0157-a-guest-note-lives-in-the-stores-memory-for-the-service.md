# ADR-0157 — A guest note lives in the store's memory for the service, and never on disk

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-29
· Relates to [ADR-0107](0107-the-buyer-is-a-subject.md),
[ADR-0035](0035-retention-and-pii-masking.md), [ADR-0056](0056-public-order-intake.md),
[ADR-0061](0061-order-relay.md)

## The problem

`docs/pos-spec.md` §3 promises that a line carries a free-text note, that the note's text never
enters the event log, and that "the kitchen reads the note from the local order record".
`sales.order_line.added` carries `note_present`, and `crate::text::GuestNote` keeps the text out of
every payload. But nothing holds the text:
- the till has no way to write a note;
- the edge sets `note_present` from the order intake and then drops the `GuestNote` it was handed;
- a marketplace or QR order's "no peanuts, allergy" is recorded as *a note existed*, and the kitchen
  never sees it.

A note is where a name and a health condition get typed. Wherever it is kept, it is personal data.

## Options considered

| | Option | Survives a restart | Personal data on disk | Backbone change |
|---|---|---|---|---|
| A | The store's `SubjectStore`, one record per noted line under the line's id | Yes | **Yes, for the retention period (365 days by default)**, and in every backup | None |
| B | **The edge's memory, bounded, for the service** | No, and a lost note is shown as lost | None | None |
| C | A store-local note table with its own short retention | Yes | For hours | A new port, and an owner review |

A keeps health data for a year to serve a need that lasts a meal. C is the right end state if
restarts during service turn out to lose notes that matter, and it needs a port. **B is chosen.** It
can grow into C later without changing what a device sees.

## Decision (proposed)

1. **A note is written with its line.** `POST` on a table's, an order's or the counter's lines takes
   an optional `note`:
   - at most 200 characters once trimmed;
   - no control characters;
   - empty means no note.

   The edge records `sales.order_line.added` with `note_present: true`, then holds the text in memory
   keyed by the line's id. A note is fixed once written. To change it, void the line and add it again,
   as for a modifier. An inbound order's `GuestNote` is held the same way instead of dropped.
2. **The memory is bounded and forgets on purpose.**
   - The edge holds at most 2,048 notes, about 1.6 MB at worst, and drops the oldest past that.
   - A note is dropped when its line is voided, or when its order leaves the open orders (settled,
     refused or closed). That is the moment the line leaves every board, so a note lasts exactly as
     long as a screen can show it.

   Nothing writes a note to the database, the outbox, a backup, a log line or telemetry.
3. **The kitchen and the till read it where they already read the line.**
   - The kitchen ticket prints the note under the item, emphasised.
   - `GET /api/orders/live` gains `note` on each line.
   - The `/ws` frame for `sales.order_line.added` gains `note` beside the event's payload. The frame
     is the edge talking to its own devices; the event in the log is unchanged.
4. **A lost note is said to be lost.** After a restart, a line's `note_present` survives in the log
   and its text does not. The till, the board and the ticket then show *"A note was written for this
   line — ask the server"* instead of nothing. A lost note is visible, never silent.

## Consequences accepted

- A note written before a restart, on a line not yet made, has to be asked for again. Edge updates
  apply in the store's maintenance window, so this is a crash or a power cut during service.
- There is no record afterwards of what a note said. If an incident needs one, the printed ticket is
  the evidence. Wanting more than that is option C.
- The `/ws` replay buffer holds a note for as long as it holds the frame (1,024 frames, in memory).

## Not decided here

Editing a note in place; notes on a whole order rather than a line; option C.
