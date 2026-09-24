# ADR-0149 — A spare box takes over by hand, and numbers its receipts above what the cloud has seen

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0025](0025-receipt-number-authority.md), [ADR-0049](0049-single-active-lease.md),
[ADR-0108](0108-the-lease-generation-is-authority.md), [ADR-0110](0110-edge-placement-is-a-deployment-axis.md),
[ADR-0113](0113-the-host-agent.md), [ADR-0124](0124-a-store-that-can-be-restored.md)

## The problem

Plan item 4.2 asks that the store PC stop being a single point of failure. Three constraints already
stand, and this record does not revisit them:

- **Nothing takes over automatically.** ADR-0113 and the roadmap reject automatic failover: a box
  that looks dead is usually a box that is still selling, and two writers produce duplicate receipt
  numbers.
- **A takeover is a lease bump** (ADR-0108), and the superseded box drains (ADR-0123).
- **A box can be rebuilt from its newest archive** (ADR-0124), as of the archive's time.

One gap turns every takeover into an audit problem. The receipt counter lives on the box. A spare
restored from an archive, or a new box with no archive, starts numbering **below** receipts the old
box has already issued, so the store prints the same receipt number twice. Receipt numbers are gapless
per store and are not legal invoice numbers (ADR-0025), but a duplicate still breaks the audit.

## Decision

1. **A spare box is an ordinary replacement, run by a person.** Stand it up (restore the newest
   archive onto it if there is one), activate it, then bump the lease: ADR-0110's order. The guide
   `docs/guides/replace-a-store-box.md` is the procedure.
2. **Every lease bump publishes a receipt floor.** The cloud publishes a Store-layer node
   `receipt_floor`, `{ "number": M }`, where M is the highest receipt number it has ingested for the
   store (from `billing.bill.settled`). Publishing it with the bump means the replacement reads it in
   the same pull that tells it it is active.
3. **The edge never allocates at or below the floor.** When the node arrives, the receipt authority
   raises its counter to at least M + 1. A counter already above M is left alone. It is one monotonic
   step, safe to repeat.

## Consequences accepted

- **A replacement never reuses a number the cloud has seen.** With a synced store, which is the
  normal case, a takeover produces no duplicate. The counter skips the numbers used after the
  archive, which is a gap in the audit rather than a collision.
- **One risk remains, and this record names it.** Receipts the old box issued **offline and never
  synced** are unknown to the cloud, and the replacement can issue the same numbers.
  - Closing that needs disjoint number ranges per lease generation (the range ADR-0049 anticipated).
  - That changes the printed number, so it needs legal to confirm the format first. It gets its own
    record, not a guess here.
- **A warm standby that follows the old box's writes is not built.** Continuous log shipping is spike
  A4 and has not been run. Until it is, the archive interval bounds what a takeover loses. A store that
  wants a tighter bound sets `backup_interval_hours = 1`.
