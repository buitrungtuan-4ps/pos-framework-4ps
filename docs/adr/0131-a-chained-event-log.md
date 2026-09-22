# ADR-0131 — The event log chains, and the cloud holds the anchor

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-22
**Extends** [ADR-0015](0015-sqlite-access.md) (the single-writer thread this makes load-bearing) · [ADR-0026](0026-port-shapes.md) (the `EventStore` contract the chain becomes part of)
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) (why a store must verify its own log with no internet) · [ADR-0040](0040-reconciliation.md) (the existing edge-initiated diff the anchor rides beside) · [ADR-0092](0092-artifact-trust-chain.md) (the other trust chain here, and why that one verifies but never signs)

**Context.** `docs/architecture.md` calls the event log append-only. That is a description of the **code**, not of the data: `store-sqlite` only ever issues `INSERT OR IGNORE`, and nothing else in the framework writes to `events`. The table it writes is three columns —

```sql
CREATE TABLE events (
    store_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    envelope TEXT NOT NULL,
    PRIMARY KEY (store_id, event_id)
) WITHOUT ROWID;
```

— and nothing in it links one row to the next. The file is SQLite on a PC in a shop. Anyone who can reach that machine can `UPDATE` an amount or `DELETE` a settled bill, and the database will report `integrity_check: ok`, because no constraint has been broken. Run against this exact schema: three settled bills totalling 2,500,000 VND become two totalling 700,000, and nothing in the file records that the other 1,800,000 was ever there.

The `event_id` does not help. A ULID gives **ordering** and **idempotency** — the two things [ADR-0026](0026-port-shapes.md) asks of it — not integrity. It is a timestamp and a random suffix; anyone can mint a plausible one. The outbox's `position` does not help either: it is assigned at commit for delivery ordering, and its rows are deleted once acknowledged.

**Options considered.**

1. **Nothing.** Defensible while the log is only ever read by the people who wrote it. It stops being defensible the moment the log is the answer to "what did this store sell", which is what event sourcing makes it.

2. **A hash chain alone.** Each record carries the hash of the one before. Cheap, no hardware, no key management. It catches a modified record and a record deleted from the middle. **It does not catch two cases**, and both were checked rather than assumed:
   - **Truncation.** Delete the last three records of the day and the surviving chain is still perfectly linked. Nothing is inconsistent; the log simply ends earlier.
   - **Recomputation.** The hash function is in the source. An attacker who edits a record and then re-derives every subsequent `prev_hash` produces a flawless chain. Verification passes.

   So a chain on its own defends against careless editing and storage corruption, not against deliberate fraud. Shipping it and calling the log tamper-proof would be worse than shipping nothing, because it would be believed.

3. **A hash chain plus an anchor the store cannot reach.** The store periodically publishes its chain head to the cloud. Both cases above then fail loudly, because a store cannot rewrite what the cloud has already seen. This is the option taken.

4. **Signatures with a key the operator does not hold** (a smartcard, an HSM, a certified secure element). This is what a European fiscal regime requires, and the reason is exactly option 2's second failure: a hash anyone can recompute proves nothing to a tax authority, while a signature over a key the shop does not possess does. **No market this framework serves is in the EU** — Vietnam, Japan, India, Cambodia, Indonesia and the United States — so NF525's certification, KassenSichV's TSE and RKSV's smartcard are all out of scope, and with them the procurement decision that was blocking this record. Deliberately left unbuilt rather than half-built; §7 requires a new ADR if a market is added that needs it.

**What the markets actually ask for, and what this is not.** Two of the six have requirements that touch this design, and both are recorded here so a later reader knows what was and was not relied on:

- **Japan.** The Electronic Books Maintenance Act (電子帳簿保存法) requires authenticity of electronic records, and one recognised route is a system in which corrections and deletions leave a trace. A chained log with a published anchor is that shape.
- **United States.** Roughly half the states criminalise automated sales-suppression software ("zappers"). A tamper-evident log is a liability posture, not a filing requirement.

Neither is a compliance claim. **Nobody may state that this framework satisfies any tax regime on the strength of this record** — that is for the company's tax advisors, per jurisdiction, before any such claim is made externally. What is decided here is the mechanism; whether it discharges a legal duty is not this document's to say.

**Decision.**

1. **Every event carries `seq` and `prev_hash`.** `seq` is a per-store counter starting at one, with no gaps. `prev_hash` is the hash of the preceding record, and the first record in a store's life chains to a genesis constant. Both ride on the envelope, so they cross every channel the envelope does and a cloud reader verifies the same bytes a store wrote.

2. **The hash covers the whole record, including its link.** `SHA-256` over `prev_hash`, `seq`, `event_id` and the serialized envelope. Covering the link is what makes the chain a chain: hashing only the payload would let records be reordered with their hashes intact.

3. **The store verifies its own chain at startup, with no internet.** [ADR-0001](0001-offline-first-store-autonomy.md) means a store that cannot reach the cloud must still be able to detect its own corruption. A break is reported and does not stop the store trading — a till that refuses to sell because yesterday's log is damaged turns a record-keeping fault into a closed shop, which is the worse failure.

4. **The chain head is published to the cloud as an event, at every shift close.** The shift boundary is chosen because it is already a reconciliation point a human attends to, and because it bounds how much history a store could rewrite unobserved to one shift. The cloud stores each anchor and refuses one that does not extend the last.

5. **Verification is a contract test, not an adapter's private business.** `pos_contract_tests::event_store` gains the chain obligations, so every implementation — SQLite, Postgres, and the fakes — is held to them, the way the existing three obligations already are.

6. **It is `seq`, not `event_id`, that orders the chain.** Events sort by ULID today, and [ADR-0026](0026-port-shapes.md) §3 already records why that is unsafe as a cursor when writers are concurrent: a chain built on ULID order would silently re-link itself the first time two transactions interleaved.

**Consequences accepted.**

- **`pos-proto` changes and the envelope grows two fields.** Additive, so no `PROTOCOL_VERSION` bump: an older reader ignores them, and the handshake already guarantees a node only sees envelopes of a version it agreed to speak.
- **Events written before this exists have no chain.** A store's chain begins at the first record written after the migration; earlier events verify as "unchained" rather than as broken. Backfilling would mean computing a chain over history nobody can vouch for, which would manufacture exactly the false confidence option 2 was rejected for.
- **The append path gains a read.** Appending now needs the previous record's hash, which is one indexed lookup inside a transaction the writer already holds. Measured before merge, not assumed.
- **A single writer per store is now load-bearing.** It already was in practice; the chain makes it a requirement, and a future concurrent writer needs the sequence assigned under the same lock that assigns `OutboxPosition`.
- **This is tamper-evident, never tamper-proof.** It does not stop anybody. It makes what they did visible afterwards, and only as far back as the last anchor the cloud holds.
