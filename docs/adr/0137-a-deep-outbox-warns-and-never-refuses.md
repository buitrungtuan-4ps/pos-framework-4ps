# ADR-0137 — A deep outbox warns and never refuses a sale

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-23
· Makes the adapters agree with [ADR-0001](0001-offline-first-store-autonomy.md) and
`docs/capacity-and-reliability.md` §5 · Relates to [ADR-0026](0026-port-shapes.md) §3 and
[ADR-0087](0087-edge-relay-and-event-publish.md)

## The problem

Both event stores refused an append once the outbox held **10,000** unsent events —
`store-sqlite`'s writer and `pos-fakes` alike, "so back-pressure behaves identically in tests and in
the field". The refusal is `RESOURCE_EXHAUSTED`, the edge maps any port failure to
`503 the store is unavailable`, and so the first append after the ten-thousandth is a till that
cannot seat a table, add a line, fire, settle or open a shift. Only reads and sign-in still work.

A bill is at least `2 × lines + 7` events. Ten thousand is roughly 430 bills at eight lines — **less
than one day** at a busy store — and it was reproduced end to end on both stores: 588 five-line
table turns and the next tap is a 503. Nothing logged the refusal, the status bar stayed green
because it reports the LAN link to the box and nothing else, and the operator saw an English sentence
with no next step.

Every document says the opposite. `docs/capacity-and-reliability.md` §5: *"no failure in this table
stops a store from selling, except that store's own hardware."* `docs/architecture.md`: calls to the
cloud *"never block a sale. Unsent events wait in the outbox."* `docs/ui-ux.md` §4: offline *"blocks
nothing"*, and past a configured threshold the answer is *"a warning banner … never an automatic
block on selling."* No document names the number 10,000.

## Options considered

1. **Raise the count.** A bigger number moves the day a cloud outage closes the shop; it does not
   remove it, and it keeps an unlogged 503 as the failure mode.
2. **Refuse on free disk space instead of a count.** Closer to the real resource, but reading free
   space needs a platform call the workspace does not have (a new dependency, and its own ADR), and it
   still ends in a refused sale — on the one day the store has been offline longest.
3. **No refusal. Warn early, warn loudly, and let the disk be the bound** — the same bound the event
   log itself has, since the outbox holds a copy of events the log already keeps.

## Decision

**Option 3.** An append is never refused because the outbox is deep.

- **The adapters.** `store-sqlite` and `pos-fakes` drop the count check. The port's documentation
  already said *"at its configured depth"*; neither adapter configures one now. The per-append
  `SELECT COUNT(*)` goes with it, which also removes a query from every sale's transaction.
- **The warning.** The edge grades the depth against a planned depth of **100,000** events — about
  nine days at the busiest store the capacity model describes, weeks at a typical one — as
  `OUTBOX_LEVEL_NORMAL` under half, `_ELEVATED` from half, `_HIGH` from four fifths and `_BEYOND`
  past it. The grade is only ever a warning: the store sells at every level.
- **The log.** A crossing upward logs once — `WARN` for the first two levels, `ERROR` past the plan —
  and returning under half logs once at `INFO`. The heartbeat's depth read feeds the same grading, so
  a box nobody is looking at still writes the day it went quiet into its log.
- **The status bar.** `GET /api/sync` reports the depth, its level and what the outbox drain last saw
  of the cloud. The till reads it and says *"Offline — selling normally"* with the count, amber from
  `_ELEVATED` and red from `_HIGH`, as `docs/ui-ux.md` §4 always specified.
- **The refusal a device does see is translatable.** Every domain refusal the edge answers now carries
  a stable `pos-error-reason` token beside its unchanged English body (`STORE_UNAVAILABLE`,
  `SHIFT_ALREADY_OPEN`, …), so a till shows the operator a sentence in their own language and a next
  step instead of a log line.

## Consequences accepted

- **A store offline for months grows its database until the disk fills**, and at that point *every*
  write fails — sales included. That is the "store disk full" row of the failure table, detected by its
  own threshold alert. It is the honest bound: the log was always going to share it.
- **The planned depth is a constant, not published configuration.** `docs/ui-ux.md` speaks of a
  *configured* threshold; publishing one is a config-schema change and is left for when a store asks
  for a different number. The constant is one line to change.
- **Back-pressure is no longer testable through the event store.** It never protected anything there —
  the thing a full outbox signalled, a cloud that is not taking events, is still visible as the depth
  and the cloud link, which is where an operator can act on it. The fakes' other queues keep their
  limits.
- **The fake's outbox is an unbounded in-memory list.** So is its event log, and has been since it was
  written; the fake is a test double and the demo's store, not a shop's.
