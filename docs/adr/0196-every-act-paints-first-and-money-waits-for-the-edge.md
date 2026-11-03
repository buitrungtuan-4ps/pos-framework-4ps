# ADR-0196 — Every act paints first, and money waits for the edge

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md) (decision 4, the allow-list) and
[ADR-0188](0188-seating-a-free-table-is-painted-at-once.md) · Relates to
[ADR-0028](0028-settlement-and-payment-invariant.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

ADR-0197 asks every tap to paint within 10 ms. Only four acts paint before the edge answers
(`ui/src/state/optimistic.ts`: adding a line, changing a quantity, moving a table, seating a free
table). Everything else, including every money act, waits for the edge's round trip before the
screen moves, and ADR-0173 requires a new ADR to add an act to the list.

The edge already does the physical part of money after it commits: a settle's drawer kick and
receipt are issued by the edge's handler once the command has committed
(`crates/pos-edge-app/src/app/shifts/cash.rs`: "the kick is the caller's, after this commits"). The
till never kicks a drawer or prints a receipt on its own.

The owner decided: every act paints at once; the drawer opens and the receipt prints only after the
edge commits; the till pre-checks the refusals it can predict; a refusal rolls the screen back with a
blocking message.

## Options considered

1. **Keep the allow-list and grow it one ADR at a time.** Money acts would never meet 10 ms.
2. **Let the till commit money locally and reconcile later.** A second writer, which ADR-0173
   decision 5 and roadmap-v3 D26 refused: receipt numbers, stock and drawers assume one authority.
3. **Paint at once for every act, by effect class, and keep the edge the only writer** (chosen).

## Decision

1. **Every act paints first.** The allow-list is replaced by the effect class each catalogue action
   carries ([ADR-0191](0191-every-act-is-in-a-typed-catalogue.md)):
   - **UI:** the paint is the act.
   - **Edge write:** paint the change provisionally and reconcile exactly as ADR-0173 decision 4
     does: the edge's answer or event confirms it; a refusal rolls it back; no answer rolls it back
     and re-reads.
   - **Money and external:** paint the act's next screen at once in a pending state, with the inputs
     locked and the amounts the edge already confirmed. Nothing derived from the act (change due,
     a new balance) is shown until the edge answers.
2. **Safeguard 1 — the physical part follows the commit.** The drawer opens and the receipt prints
   only after the edge has committed the command, and only from the edge, as today. A pending screen
   never kicks a drawer, prints or tells the cashier to hand over change.
3. **Safeguard 2 — predictable refusals are checked before the paint.** Each money action lists the
   refusals the till can decide from what it holds: the person's permission, an open shift, a drawer
   not yet counted, a tendered amount that covers the balance, a tender the channel accepts, no
   write still pending on the bill, an approval where the ceiling needs one. A check that fails shows
   the refusal at once and paints nothing else. The edge still checks everything.
4. **Safeguard 3 — a refusal blocks.** When the edge refuses a money act, the screen rolls back to
   where it was and shows the refusal's own sentence in a dialog the operator must dismiss. A toast
   is not enough for money. What the operator typed is kept (`docs/ui-ux.md` §1 principle 7).
5. **The edge stays the only writer.** No act is queued on the device or re-sent later (ADR-0173
   decision 5). Totals and tax remain the edge's.
6. **ADR-0188 holds as it is.** Seating is an edge write under decision 1, and payment still waits for
   the confirmed seat, now as a predictable refusal under decision 3.

## Consequences accepted

- **A pending money screen can be rolled back.** The cashier may see the receipt screen for the
  length of a round trip and then a refusal. This is accepted because the drawer and the paper wait.
- **The predictable checks are a copy of the edge's.** They can drift; the edge's check remains the
  authority, so drift costs a later refusal, never wrong money. A test per money action runs both
  against the same cases.
- **The four-act allow-list in `ui/src/state/optimistic.ts` is retired** in phase 2, as each act moves
  to the catalogue.
