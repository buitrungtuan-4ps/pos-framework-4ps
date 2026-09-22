# ADR-0132 — The cloud recomputes the chain from the events it holds

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-22
**Amends** [ADR-0131](0131-a-chained-event-log.md) — option 3 there says a published anchor makes *both* of the chain's blind spots "fail loudly". Half of that is now built and true; the other half is not, and this record says which is which rather than leaving the claim standing.
**Relates to** [ADR-0022](0022-events-partition-strategy.md) (the partition key this reads by) · [ADR-0026](0026-port-shapes.md) (at-least-once ingest, and why a gap is not a fault) · [ADR-0040](0040-reconciliation.md) (what fills a gap)

**Context.** [ADR-0131](0131-a-chained-event-log.md) named two things a hash chain cannot catch alone — **truncation** and **recomputation** — and answered both with one mechanism: publish the head to the cloud, which a store cannot rewrite.

Building it showed the answer is not symmetric.

- **Recomputation is caught.** A store that rewrites its history and re-derives every later link arrives at a *different* head for a length the cloud already holds. One head per chain length per store, and the second one is refused.
- **Truncation is not.** A store cut back to length 20 and then traded on publishes its next anchor at length 61 — *above* anything the cloud holds. Nothing collides. From the anchors alone it is honest growth.

The evidence for the truncated history is not in the anchors at all. It is in the **events**: the cloud holds the original records at seqs 21–61 and then receives new, different records claiming those same seqs. Two events at one position in one store's chain is not an ambiguity — it is the fork, in the open, in data the cloud already has and is not reading.

**Options considered.**

1. **Leave it.** Defensible only if the limit is stated everywhere the mechanism is described, which it now is. It is not defensible as an end state: the case it misses is the deliberate one, and a store's own chain has no way to notice.

2. **A per-event position table** — one row per event, keyed `(tenant, store, chain_seq)`, written at ingest, refusing a second event at a taken position exactly as `chain_anchors` refuses a second head. Simple, and the refusal lives in a primary key. Rejected on cost and on retention: it doubles the row count of the largest table in the system, it is not partitioned the way `events` is ([ADR-0022](0022-events-partition-strategy.md)), and it would grow without a retention story — which is its own §7 decision, taken to buy a check the next option gets for nothing.

3. **Recompute the chain from the events the cloud already holds, at the anchor.** The anchor names a length; the previous anchor named the one before it; between them lies exactly the window the store is asking the cloud to believe. Read those events, sort by `seq`, and check the chain the store claims against the chain its own records make. This is the option taken. It costs nothing on the write path, adds no table, and is strictly stronger than option 2 — position collision is one of four things it finds, and the others were not otherwise going to be found at all.

**Decision.**

1. **An anchor is checked against its own envelope first, before anything is read**, for as much as that envelope can settle — which is less than it first appears, and the limit is the decision.

   The edge reads its head and *then* commits the anchor ([ADR-0131](0131-a-chained-event-log.md); a record cannot contain its own hash), so a sale landing between the two leaves the anchor event several records above the length it reports. `prev_hash == chain_head` therefore holds only when the anchor event sits immediately after the length it names. So: the envelope's `seq` must be **strictly greater** than the payload's `chain_seq` — an anchor claiming a chain at least as long as the record carrying it is impossible, and is refused on that one event with no query. When the two are adjacent, `prev_hash` must also equal `chain_head`. When they are not, the head is checked against the window instead, where it is checked anyway.

   Stated this way because the tempting version — "the anchor's envelope proves its payload" — is false under concurrency, and a check that is right except when a till is busy is a check that is wrong.

2. **The window is the events between the last accepted anchor and this one**, read by `(tenant_id, store_id, business_date)` — the index [ADR-0022](0022-events-partition-strategy.md) already provides — and ordered by `seq` in memory. No column is promoted onto `events` and no index is added to it: the log is the most-written table in the system, and a verification that runs once per shift per store must not tax every insert to make its own read tidier. Nothing is filtered on a JSON operator either; the day is the filter, and the chain is read out of the envelopes in Rust.

3. **It reads through a cloud seam, not through `EventStore`.** `EventQuery` pages a store's log by `event_id` and has no notion of a trading day; adding one would change a port every adapter implements, to serve one cloud-side question. [ADR-0040](0040-reconciliation.md) already set this precedent — the reconciliation diff is a `ReconcileStore` in `pos-cloud` filled by `store-postgres`, not a thirteenth obligation on `EventStore` — and this follows it. **`pos-ports` and `pos-proto` are untouched.**

4. **The window is bounded twice, and an unmet bound is reported rather than silently trimmed.** By events, because a shift is thousands and not millions; and by days walked back, because a window that begins before the anchor's own trading day is read by stepping back one day at a time. A claim that exceeds either cap is recorded as *unverified — window too large*, which is a finding a human reads, never a pass. A cap hit means something is wrong with the claim, not with the cap.

5. **Four findings, and only two of them are accusations.**
   - **Forked** — two events at one `seq`. This is the truncation case, and it is what this record exists for.
   - **Broken link** — an event whose `prev_hash` is not the hash of the record before it. The cloud re-derives the chain from its own copies rather than trusting that the store did.
   - **Incomplete** — a `seq` missing from the window. **Not a fault.** Ingest is at-least-once and a broker gap is ordinary; [ADR-0040](0040-reconciliation.md) exists to fill exactly this. Reported so the gap is visible and re-pushed, never as tampering.
   - **Unverified** — the window was too large, or the cloud holds no chained events for it. An honest absence of an answer, distinct from a pass.

6. **A finding never fails an ingest and never refuses an event.** The same rule the anchor ledger already follows, for the same two reasons: what a store sent is the evidence, and an outage that halted ingest is the cover somebody tampering would want.

7. **It runs where the anchor is judged**, not on a sweep. The anchor is the moment the question is asked and the moment the window is defined; a background scan would have to invent both.

**Consequences accepted.**

- **The guarantee still reaches back only to the last anchor.** This widens *what* is checked within a window, not *how far back* the cloud can see. A store with no anchors is unverifiable, exactly as before.
- **A store that truncates below its last anchor and never trades past it is still not caught here.** Its next anchor collides and is refused ([ADR-0131](0131-a-chained-event-log.md)); its missing records read as **incomplete**, which is indistinguishable from a broker gap. That ambiguity is real and is not dressed up: an incomplete window is a prompt to reconcile, and a window that stays incomplete after reconciliation is what a human should be looking at.
- **The cloud now computes chain hashes.** `pos-cloud` already links `sha2`, and the preimage is [`pos_proto::chain`]'s — the same bytes the edge hashed, which is the point. No new dependency, and no second definition of what is hashed.
- **This is still tamper-evident, never tamper-proof.** It does not stop anybody. It makes more of what they did visible, and only within a window an anchor closed.
