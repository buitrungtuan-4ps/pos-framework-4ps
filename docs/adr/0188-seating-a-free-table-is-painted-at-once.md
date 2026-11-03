# ADR-0188 — Seating a free table is painted at once

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md) (decision 4, the allow-list
of optimistic writes) · Relates to [ADR-0109](0109-counting-the-taps-an-operator-makes.md)

Accepted by the owner on 2026-11-03 ("ok hết, làm luôn"), as part of a target of visible feedback
within 30 ms of a tap.

## Context

ADR-0173 decision 4 lets three acts paint before the edge answers: adding a line, changing a
quantity and moving a table. Tapping a free table is not on that list. The floor awaits the seat
before it navigates, so the most common first tap of a service waits one round trip before anything
moves (`ui/src/screens/Floor.tsx`).

## Decision

1. **Seating a free table joins the allow-list.**
   - **On the tap:** the till marks the table as seating and opens the Order screen at once. Lines
     the server adds there are provisional, as decision 4 already allows.
   - **When the edge answers:** its answer or its fan-out event, whichever lands first, confirms the
     seat and the order's id, matched by the request.
2. **A refusal takes the server back.**
   - `BILL_ALREADY_OPEN`, for example when another till seated the table first, returns to the floor
     with the refusal's sentence.
   - The provisional lines are kept in a note on the screen, so that nothing typed is lost silently.
   - No answer: re-read the floor and the table's order, as decision 4 does.
3. **Totals and payment stay the edge's.**
   - Payment is disabled until the seat is confirmed, as decision 4 disables it while any write is
     pending.

## Consequences accepted

- **Two tills on one free table:** if two tills seat the same table at the same moment, the second
  shows the Order screen for a few milliseconds before it is sent back. This is accepted for a tap
  that moves at once on every other seat.
- **Changes are in `ui/` only:** no edge, port or protocol change.
