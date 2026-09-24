# ADR-0145 — The edge keeps an event until it is synced **and** N days old

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0131](0131-a-chained-event-log.md), [ADR-0107](0107-the-buyer-is-a-subject.md),
[ADR-0137](0137-a-deep-outbox-warns-and-never-refuses.md), [ADR-0143](0143-the-device-credential-syncs-and-events-travel-over-https.md)

## The problem

The documents promise that "each store retains 90 days of events, so a cloud data loss within that
window is recoverable" (`docs/architecture.md`, `docs/capacity-and-reliability.md`).

**What that promise is about.** It covers the edge's **event log**: every fact the store recorded
(each sale, payment, void, shift, and the audit events among them, such as a manager's override). It
is kept on the store PC so that the cloud's copy can be rebuilt from the stores. It is not a separate
audit trail, and it is not the retention of personal data. Personal data lives in subject records
that ADR-0107 masks after the country's `retention_days`, whatever happens to the log.

**No retention was ever built.** Nothing deletes an event. The database and the start-up replay grow
for as long as the store trades, while the documents describe a 90-day window.

## Decision

1. **An event is deleted only when all of these hold:**
   - **Synced.** The event link acknowledged it: it was committed before the outbox's first
     unacknowledged row.
   - **Old.** Its `event_time` is more than **N days** ago.
   - **Nothing open depends on it.** It is older than the oldest open business (an order still owing,
     an open bill, a table that is not free, a guest order awaiting staff, the open shift), less one
     day of margin for a clock that stepped.

   An offline store therefore keeps everything until it syncs, however old.
2. **Deletion takes a prefix in commit (`seq`) order, and the newest chained record is always kept.**
   The store records a **checkpoint** (the `seq` and `hash` of the last deleted record) in the same
   transaction as each deletion, and `verify_chain` starts there. The chain stays checkable end to
   end over what is kept, and the next record never restarts at `seq` 1.
3. **N is set per store in the cloud.** It lives on a Store-layer `retention` node,
   `{ "event_log_days": N }`, published from the console's store settings. The default is 90 and the
   allowed range 30–3650. A store that has never received the node uses 90.
4. **The sweep runs hourly, beside the subject-masking sweep.** It deletes at most 2,000 records per
   transaction, so a sale never waits long behind it on the single writer.
5. **The file stops growing; it does not shrink.** SQLite reuses freed pages (there is no
   `auto_vacuum`). Shrinking a file is an operator's step, not the sweep's.

## Consequences accepted

- **"The log is never rewritten" still holds.** No record is edited. Old ones age out, the
  checkpoint says where the kept log begins, and ADR-0131's guarantees apply from there.
- **The cloud's rebuild window per store is that store's N.** The other half of the promise, a command
  that asks the edges to replay from a given id, **does not exist yet**. This record makes the first
  half true, not the second, and the architecture document now says so.
- **"Synced" means the link's acknowledgement.** Over HTTPS (ADR-0143) that is the cloud's ingest.
  Over JetStream it is the stream's acceptance, and N days is the margin for the difference.
- **Receipt numbers, queue numbers, pairings, sign-ins and the lease are untouched.** They live in
  their own tables, not in the log.
- **`ChainAnchor.seq` and `chain_seq` now name a position, not a count of records.** The cloud already
  reads them as positions. Their doc comments are corrected when those crates next change.
