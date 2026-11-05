# ADR-0205 — A refusal any act can meet is not listed per act

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-04
· Amends [ADR-0191](0191-every-act-is-in-a-typed-catalogue.md) decision 1
· Relates to [ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0173](0173-the-till-declares-each-piece-of-store-data-once.md)

Accepted by the owner on 2026-11-05.

## Context

ADR-0191 gives each catalogue entry "the refusals the till can predict before sending". Comparing
the entries with what the edge's commands return found two kinds of gap. Some were refusals of one
act's own command that the entry left out, such as a settle's `DRAWER_DAY_ENDED`; those are now
listed. The rest are refusals that no entry lists, because they do not belong to any one act:

- `APPROVAL_REFUSED` and `APPROVER_LOCKED_OUT` come from the shared approval step. Every act that
  takes a manager's code and PIN can meet them, and the edge decides them by checking the PIN and
  counting failures. The till holds neither.
- `TRANSITION_REFUSED` comes from the domain's state machines. It means the subject is no longer
  in a state the act applies to, usually because another device changed it first. Any act on a
  bill, a line, a table or a shift can meet it.

The catalogue already leaves out `PERMISSION_DENIED`, which `permissions` covers, and a test keeps
it out. ADR-0191 does not say whether these three are left out on purpose.

## Options considered

1. **List them on every entry they can reach.** Most entries would carry the same three lines,
   which adds no information. A till would also read them as checks it could make before sending,
   and it cannot.
2. **A separate list of general refusals in the catalogue.** This adds a new field to `pos-proto`
   and to the snapshot to record something that is true of every act.
3. **Leave them out of every entry, on purpose, and say so** (chosen).

## Decision

1. **An entry's `refusals` lists only what is particular to that act.** These are refusals its own
   command raises from facts the till can hold. A refusal that any act of a kind can meet, and that
   the till cannot decide before sending, is not listed on any entry: `APPROVAL_REFUSED`,
   `APPROVER_LOCKED_OUT` and `TRANSITION_REFUSED`, as `PERMISSION_DENIED` already is.
2. **The till handles them like any other refusal from the edge.** A money act rolls back with the
   refusal's own sentence in a blocking dialog (ADR-0196 decision 4). An edge write rolls back and
   reads again (ADR-0173 decision 4). The till's translation of each token stays as it is.

## Consequences accepted

- An entry does not list every refusal its route can return. A package author reads these three
  from this ADR, not from the snapshot.
- A refusal that is particular to an act must still be listed on its entry. Leaving one out is a
  mistake to fix, not a case of this decision.
- Once this ADR is accepted, a test in `pos-proto`'s `actions::tests` can keep the three tokens out
  of every entry, as it does `PERMISSION_DENIED`.
