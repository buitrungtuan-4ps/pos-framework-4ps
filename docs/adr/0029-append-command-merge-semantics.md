# ADR-0029 — Merge is not uniformly last-writer-wins: terminal states win

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-19
**Amends** [`docs/pos-spec.md`](../pos-spec.md) §3 · **Relates to** [ADR-0028](0028-settlement-and-payment-invariant.md)

**Context.** `pos-spec.md` §3 and `architecture.md` §2 make order writes **append commands that
merge**, with edits to the same line resolved last-writer-wins and both versions kept in the audit
log. That is what lets two devices work one table without a lock.

Uniform last-writer-wins has a hole. Device A voids line 7 at `t1`; device B edits line 7's quantity
at `t2 > t1`. Pure LWW makes B's edit win, and **line 7 is no longer voided** — a cancelled item
returns to the bill and the guest is charged for it. The archive is emphatic that the design
philosophy is *conflict cannot happen* (every data type has one authority), and same-line editing is
the one deliberate exception. An exception that silently un-voids an item is the wrong exception.

**Decision.** Merge has two layers.

- **State transitions merge by the state machine's partial order. Terminal states always win.**
  `VOIDED` and `SETTLED` cannot be overwritten by a later non-terminal edit, whatever its timestamp.
  This is the same rule ADR-0028 applies to `bill:settle`; it simply also applies per line. The
  `StateMachine` framework in `pos-core` marks terminal states, and the merge consults that mark
  rather than re-deriving it.
- **Non-state fields merge last-writer-wins, keyed on `(event_time, device_id)`.** The device id is
  the tiebreak, so two edits in the same millisecond resolve deterministically rather than by
  arrival order. Quantity, note-presence, modifiers, seat — everything that is not the line's state —
  takes the higher key.

Both versions stay in the audit log, as already required.

**Why this needs a property test, not just a fix.** Merge must be **commutative and associative**, or
two devices replaying the same events in different sync orders converge to different states — a
data-correctness bug that appears only under concurrency and is very hard to reproduce from a store's
report. So `pos-core` asserts, by property test over arbitrary event sets and orderings:

1. `merge(a, b) == merge(b, a)` (commutative);
2. `merge(merge(a, b), c) == merge(a, merge(b, c))` (associative);
3. a `VOIDED` line is never observed as un-voided by any ordering of a void and a later edit.

Commutativity and associativity together mean the fold over any permutation of a line's events yields
one state, which is exactly "conflict cannot happen" restated as a checkable law.

**Scope.** Same-line editing stays the **only** place this merge machinery lives. The rule to carry
forward: if a design ever reaches for CRDT-style merge anywhere else, that is a signal the
one-authority model has been broken, not that better merging is needed. A new call site for this
module is a review red flag by construction.

**Consequences.**

- The terminal-state mark is load-bearing, so it belongs to the state machine definition and is
  covered by the same exhaustiveness test that proves no undefined state is reachable.
- `(event_time, device_id)` is a total order only if device ids are unique, which they are (a ULID
  per device). A missing device id is not representable in the envelope, so the tiebreak cannot
  degrade to arrival order by accident.
- `pos-spec.md` §3 is amended from "last-writer-wins" to "terminal states win; other fields
  last-writer-wins on `(event_time, device_id)`", so the specification states the rule the code
  enforces.
- The merge is pure and lives in `pos-core`, so it runs identically at the edge (two devices on a
  table) and in the cloud (rebuilding a projection from the event log). One implementation, one law.
