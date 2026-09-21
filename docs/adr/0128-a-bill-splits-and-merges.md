# ADR-0128 — A bill splits and merges

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-21
**Extends** [ADR-0028](0028-settlement-and-payment-invariant.md) (the settlement invariant each resulting bill must still prove)
**Relates to** [ADR-0024](0024-protocol-version-negotiation.md) (why two enum values are additive rather than a bump) · [ADR-0115](0115-reason-codes-are-a-managed-list.md) (why neither act is a void) · [ADR-0029](0029-append-command-merge-semantics.md) (the line order the parts inherit)

**Context.** `billing.bill.split` and `billing.bill.merged` have been defined in `pos-proto`, with their exact shapes, since the schema was written. **Nothing has ever emitted either.** The `Bill` state machine's own doc comment anticipates them — *"one order may split across several bills, and several orders may be merged into one"* — and the split event's doc even states the arithmetic it intends:

> The parts sum exactly to the original, because splitting partitions amounts that were already allocated and snapshotted rather than recomputing percentages.

Every piece of that sentence is true of the design and none of it is true of the code. A table of six who want to pay separately is, today, either six tables opened as a workaround or one bill and a pile of arithmetic done on paper beside the till. It is the largest single gap between this till and the products it is measured against.

Two things stand in the way, and both are structural rather than a matter of writing a route:

1. **A bill has no lines.** `BillRecord` is `{ order_id, table_id, state }`, and the amount owed is assembled by reading *every* line of the order (`taxable_bases`). There is no way to say "this bill covers these lines", so there is nothing to partition.
2. **A bill has nowhere to go.** `BillState` is `OPEN`, `SETTLED`, `VOIDED`. A bill whose lines have moved into two others is none of those, and `SETTLED` being terminal is precisely what makes a second settle impossible — *"a property of the type rather than of a lock"*, as the machine puts it.

Because the second needs a `pos-proto` enum value, `AGENTS.md` §7 requires this record before the code.

**Decision.**

1. **A bill covers a set of order lines, not an order.** `BillRecord` gains the `OrderLineId`s it bills, and the taxable bases are assembled from *those* lines rather than from the order's. `open_bill` takes every unvoided line on the order, which is exactly today's behaviour written down rather than inferred. Nothing observable changes until a bill is split, and every later decision here depends on this one.

2. **`BillState` gains two terminal values, `SPLIT` and `MERGED`.** Additive: the `*_UNSPECIFIED` zero value is untouched, no value is removed or renamed, and `PROTOCOL_VERSION` is **not** incremented — a bump would force two protocol versions to run in parallel for two releases ([ADR-0024](0024-protocol-version-negotiation.md)) for values nothing older reads. An older reader maps an unknown token to the unspecified variant, which is "a state I do not know" rather than a parse failure.

   **Why states and not projection bookkeeping.** The edge could remember "this bill was split" beside the machine and refuse to settle it there. That would put half of a bill's lifecycle in the machine and half next to it — and the half next to it would be the half that decides whether a guest can be charged for the same food twice. The refusal belongs where `SETTLED`'s refusal already is.

   **Why two values and not one `CLOSED`.** They are different facts with different evidence. "Where did this bill go?" is answered for a split source by its parts and for a merged bill by its target; the audit trail, the reprint and the operator asking at the till all need to tell them apart. One value would make two answers into one.

3. **A split is a partition, and the domain checks that it is one.** `BillCommand::Split { parts }` is refused unless there are at least two parts, no part is empty, every line of the source appears in exactly one part, and no line appears that the source does not bill. The source moves to `SPLIT`; one new bill opens per part, each covering its own lines.

   Not "carve one off and keep the rest": a partition has one rule to state and one to check, while the asymmetric form leaves *"and what does the source hold now?"* as a second question needing its own answer. A server splitting one guest off a table of six performs a two-part split, and the six-line part is an ordinary new bill.

4. **Each resulting bill computes its own tax and its own rounding.** `billing::assemble` runs per bill over that bill's captured line totals. The alternative — allocate the source's rounded total across the parts — needs a residual rule, and the residual would land on whichever guest the code happened to sort last.

   The consequence is worth stating plainly rather than hiding: **the parts' totals can differ from the source's by the cash rounding on each part**, because rounding is materialised on a total (ADR-0028) and there are now several totals. That is the correct answer, not a discrepancy. The source's total was quoted and never charged; what the store banks is the sum of what each guest was actually asked for. The event's "parts sum exactly to the original" is a statement about the *line amounts* being partitioned rather than recomputed, and that property holds exactly.

5. **A merge folds bills into a target, and the target survives.** `BillCommand::Merge { absorbed }` requires every absorbed bill to be `OPEN`; the target takes their lines, and each absorbed bill moves to `MERGED`. The target keeps its identity — it is the bill the cashier is standing in front of — so a merge is not a split in reverse and does not need to be.

   **The first implementation restricts a merge to bills on the same table.** The model does not: a bill covers lines, lines belong to orders, and nothing in the shape prevents one bill from covering two orders' lines — which is what "several orders may be merged into one" has always meant. What stops it *today* is the floor: settling a bill moves its table from `AwaitingPayment` to `NeedsCleaning`, and a bill spanning two tables makes "which table moved?" a question with two answers. That is a floor-cycle decision, and it gets its own record when a store asks for it.

6. **Neither act requires a permission, and neither requires a manager.** A void needs both because it forgives money; a discount will need both because it reduces money, and `billing.discount.apply` and `billing.comp.apply` are already published for it. A split partitions amounts that are already captured and a merge concatenates them — **no money is created, forgiven or moved out of the store by either**, and the totals reconcile by construction. Adding a permission for an act that cannot lose money would be the speculative layer `AGENTS.md` §2 forbids, and a PIN prompt in the middle of the busiest flow on the floor would be paid for on every table, every service, for nothing.

7. **A settled or voided bill can be neither split nor merged, and this adds no exception.** Both are already terminal. Taking money back after payment is a refund, which [ADR-0028](0028-settlement-and-payment-invariant.md) deliberately puts outside this machine as its own signed event.

8. **A resulting bill is an ordinary bill, so splitting again is ordinary.** A table that splits three ways and then finds two of the three want to split again performs two operations, not a special case. Nothing tracks a split "depth".

9. **A bill's line set is recorded in the log, not only in the projection.** `billing.bill.opened` gains `order_line_ids` — additive, on an event that already carries `bill_id` and `order_id`.

   This was missing, and it is the decision this record was amended to add. Without it the log says *which bills* a split produced and never *which lines went into which*, so the property the split event states about itself — *"the parts sum exactly to the original"* — **cannot be checked from the store's own log.** Only a live projection would know, and a projection is a cache. That breaks `AGENTS.md` §1 rule 4 (events are the source of truth) before it breaks anything an auditor cares about: a store replaying its own log after a restart could not rebuild which bill owed what.

   On `bill.opened` rather than on `bill.split`, because a bill covers lines from the moment it exists — decision 1 — and putting the partition on the split event would make the same fact true in two places for bills born two different ways. A merge needs no new field: the target's set is its own plus every absorbed bill's, and both are already in the log.

10. **A split commits as one transaction, and nothing is ever edited.** The resulting bills' `bill.opened` events and the `bill.split` that names them are appended together or not at all, so the log never holds parts whose parent is missing, or a parent whose parts are. And undoing a split is a **new merge**, not a restoration: the source bill stays `SPLIT` for ever. That is the inalterability principle every cash-register regime states in its own words, and it is why the two terminal states of decision 2 are terminal.

**What an auditor can reconstruct, and what they cannot.**

The question this record was amended to answer: *can a bill still be traced through split → merge → split?*

**Yes, forwards, and the chain always terminates.** `SPLIT` and `MERGED` are terminal, so a bill appears as a split's source or a merge's absorbed member **once**. The lineage is therefore a directed acyclic graph by construction — there is no cycle to walk into, and no "depth" to track (decision 8). A merge's *target* stays `OPEN` and may be split later; the log's order tells those apart.

With decision 9, each step is checkable arithmetic rather than a claim:

- a split: the parts' `order_line_ids` partition the source's exactly — no line lost, none duplicated, none invented;
- a merge: the target's set afterwards is the union of its own and the absorbed bills';
- either way the money follows, because a line's amount was captured when it was added (§14.2) and no reduction moves between bills.

**Backwards is a scan, not a link, and that is accepted.** Given a bill, "what was I split from?" is answered by searching the `bill.split` events for one naming it. A `parent_bill_id` on `bill.opened` would make it a link, and is deliberately not added: it would be a second encoding of a fact the split event already carries, and the two could disagree. An audit reads a log in order; it does not need random access.

**No receipt number is consumed by either act.** A number is allocated at settle and only at settle (ADR-0025), so the store's gapless sequence is untouched by any amount of splitting. This is the property a tax audit actually tests, and it is the reason splitting is not a revenue-suppression route: the parts each settle, each takes its own number, and the numbers have no gaps.

**Two things this record does not fix, named rather than implied.**

- **The event log is append-only but not *chained*.** `EventEnvelope` carries an id, a timestamp, a business date and a schema version — no sequence number and no hash of the previous record. France's NF525, Germany's KassenSichV/DSFinV-K, Austria's RKSV and Portugal's SAF-T PT all require chaining precisely so that a *deleted* record is detectable, and this log would not detect one. That is a property of the whole log rather than of splitting, it touches every event and the sync protocol, and folding it in here would be the speculative widening `AGENTS.md` §2 forbids. **It needs its own record.** It is also not what Vietnam's Decree 123/2020 and Circular 78/2021 or Japan's qualified-invoice system ask for — both put their integrity requirement at the *invoice*, which ADR-0025 and [ADR-0005](0005-country-neutral-core.md) already place behind the country module — so this is a gap against the strictest regimes rather than against the two this product trades in.
- **`order_line_ids` on `bill.opened` grows with the order.** A bill covering a hundred-line banquet carries a hundred ULIDs. Accepted: the alternative is deriving it, and a derivation is exactly what decision 9 exists to stop.

**Consequences accepted.**

- **The projection grows.** Each bill carries its line set, and the replay fold gains `billing.bill.split` and `billing.bill.merged`. A store replaying its own log reconstructs which bill owed what — which it could not do before, because the answer was inferred from the order.
- **Two enum values reach the cloud before the cloud reads them.** A store on a newer build emits `BILL_STATE_SPLIT`; an older cloud maps it to unspecified and stores it. That is the same degradation every additive enum value in this tree has, and it is why the zero value exists.
- **The snapshot files move.** `docs/snapshots/routes.txt` gains the two routes; `events.txt` is untouched, because both events were already published; `permissions.txt` is untouched, per decision 6.
- **Splitting evenly by N is not this, and is deliberately absent.** "Split the bill four ways" is what a great many people mean by the phrase, and it **cannot be expressed as a partition of lines** — four equal shares of a seven-line bill do not correspond to any grouping of those seven lines. It is a different operation with different arithmetic: allocate a total across N payers, with a residual rule, and it interacts with discounts and with who is owed the tax invoice. Folding it in here would break the one property the split event states about itself, so it needs its own record and its own event. Until it exists, an even split is several payments against one bill, which the settle path already supports.
- **The screen is not decided here.** Dragging lines into a second panel, a "split by seat" button that computes the partition from the seat each line carries (#366), and what the cashier sees while two bills are open are all UI decisions that follow this model. This record fixes what a split *is*; `docs/ui-ux.md` §3 will say what it looks like.
