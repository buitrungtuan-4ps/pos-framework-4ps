# ADR-0204 — A money act with no answer is never rolled back blind

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-03
· Amends [ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md) decision 1
· Relates to [ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md),
[ADR-0028](0028-settlement-and-payment-invariant.md)

Proposed: it waits on the owner's acceptance.

## Context

ADR-0196 paints a money act's next screen at once, pending, and says what happens when the edge
commits or refuses. It does not say what happens when the edge does not answer: the request timed
out or the connection dropped. For an edge write, ADR-0173 decision 4 rolls the change back and
reads again. For money that is unsafe. The settle may have committed, and the edge then kicked the
drawer and printed the receipt. A till that rolls back shows the bill unpaid with the cash already in
the drawer, and invites the cashier to take the payment again.

## Options considered

1. **Roll back and re-read, as for an edge write.** Simple, but for the length of the re-read the
   screen says "not paid" about a bill that may be paid, and a second tap charges twice.
2. **Stay pending until the edge says what happened** (chosen).

## Decision

1. **A money act with no answer stays pending.** The screen keeps the act's pending state with the
   inputs locked and says "Checking with the store server…". The till reads the act's subject (the
   bill, the shift) again until the edge says either that the act committed, which then shows as done,
   or that it did not, which rolls back with the blocking refusal of ADR-0196 decision 4.
2. **The re-read is bounded.** It retries every 2 seconds for 30 seconds. After that, the screen says
   the store server cannot be reached and that the payment is not confirmed, and keeps reading every
   10 seconds while it is open.
3. **A manager may leave it to look up later.** A person with the act's permission and a manager's
   PIN may leave the pending screen. The subject stays marked on this till as **unconfirmed** until
   the edge answers for it. A bill marked so offers no settle, here or from the order screen; the
   other tills see it as the edge does.
4. **Never a second attempt.** While an act's answer is unknown, the till does not offer that act on
   its subject again. The edge stays the only writer (ADR-0196 decision 5), and nothing is queued or
   re-sent.

## Consequences accepted

- A cashier can wait on a pending payment screen for as long as the edge is unreachable, rather than
  being told a payment failed that may have succeeded.
- A manager's PIN is needed to walk away from an unconfirmed payment.
- The till keeps a small per-subject "unconfirmed" mark until the edge answers. It is lost on reload,
  and the reload reads the edge's state anyway.
