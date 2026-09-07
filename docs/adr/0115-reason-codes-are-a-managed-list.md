# ADR-0115 — Reason codes are a managed list, with a framework default a store can never be without

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-07
**Relates to** [ADR-0001](0001-offline-first-store-autonomy.md) (why an absent node cannot be allowed to stop a void) · [ADR-0070](0070-people-and-access.md) / [ADR-0072](0072-floor-and-kitchen.md) / [ADR-0079](0079-inventory-and-suppliers.md) (the author-in-cloud → publish config node → edge-applies pattern this reuses) · [ADR-0071](0071-config-without-json.md) (the config tree the node lands on) · `docs/pos-spec.md` §11 item 2 (the requirement, and that it is a **fraud control**), §5 (fire, void), §6 (discount, comp, refund), §7 (the drawer), §12 (why a code and not free text) · [`docs/roadmap-v3.md`](../roadmap-v3.md) B2.2

**Context.** `docs/pos-spec.md` §11 **Fraud controls**, item 2, names the requirement plainly:

> **Mandatory reasons** from a cloud-managed list for: voiding fired lines, discounts, comps, refunds, bill voids, opening the drawer outside a sale.

Six actions, one list, and **the list does not exist.** `ReasonCodeId` is declared in `pos_proto::ids` and appears in exactly three places: two port signatures (`pos-ports/src/dynamic.rs`, `pos-ports/src/delivery.rs`) and test fixtures that invent one with `ReasonCodeId::new(Ulid::from_u128(1))`. No config node carries reason codes, the cloud authors none, the edge syncs none. Meanwhile `sales.order_line.voided` and `billing.bill.voided` both declare `reason_code_id` with the doc comment *"The mandatory reason, from the cloud-managed list"* — a field whose contract cites a list nothing produces.

This is the same shape as the gap ADR-0079 closed for inventory: a finished domain with no inputs. `pos_core::decision::decide_line` already handles `LineCommand::Void`, steps the `OrderLine` machine to `Voided`, gates a void-after-fire on `Permission::VoidFiredLine`, requires a verified PIN because that permission is PIN-flagged, and emits the void-ticket effect. All of it is unreachable: no HTTP route on the edge reaches it, and could not, because it would have no valid reason code to put in the event.

Eleven config nodes already exist (`floor`, `stations`, `tax`, `locale`, `campaigns`, `inventory`, `channels`, `tender`, `qr`, `permissions`, `capabilities`). A twelfth is not a new idea; it is the obvious one.

**Decision.** A reason code is an entry in one managed catalogue, tagged with the actions it is valid for, published to the store as the `reason_codes` config node — **over a framework default set the binary always carries.**

1. **One catalogue, not six.** The spec says *a* list for six actions, and an operator thinks in reasons ("waste", "wrong item", "staff error"), not in six parallel lists. So there is one catalogue, and each entry declares `applies_to`: the subset of actions it may be given for. Offering "opened to make change" as a reason for voiding a fired line is how a reason field becomes noise, and noise is how a fraud control stops being one.

2. **A wire node + a pure conversion** (slice 1, this ADR). `pos_proto::reason_codes::PublishedReasonCodes` is the serializable mirror the cloud writes as the `reason_codes` key on the config tree's Store layer: entries of (`id`, a short stable `code`, `display_name`, per-locale `translations`, `applies_to`, `active`). `ReasonAction` is a new wire enum with exactly the six the spec names. The edge turns it into a lookup that answers one question — *is this id valid for this action?* — which is the only question the domain needs.

3. **The framework default set is in the binary, and absence is not a brick.** Every other node means "feature off" when absent. This one cannot: a store must be able to void a mis-keyed line during a cloud outage, on its first day, before anyone has opened the console. A node that gates voiding on cloud reachability would make an unreachable cloud into a store that cannot correct a mistake — and the whole point of [ADR-0001](0001-offline-first-store-autonomy.md)'s offline-first posture is that the store keeps trading.

   So `PublishedReasonCodes::framework_default()` ships a small set in `pos-proto`, the edge starts with it, and a published node **replaces** it wholesale. The event therefore always carries an id from a real, enumerable list, and the list's provenance is honest: baked in until an operator publishes their own.

   *The default set's wording is a business decision, not an engineering one.* The initial entries are deliberately minimal and generic — waste, wrong item, customer changed their mind, staff error, test transaction, making change — and Pizza 4P's operations should review them before the pilot. What this ADR fixes is that there **is** a set, that it is enumerable, and that publishing replaces it.

4. **Validation happens at the edge, before the event.** The route rejects an id the synced list does not hold for that action, with the AIP-193 envelope naming the field and what it accepts (the Q3b-4a shape). This is what makes the two events' *"from the cloud-managed list"* true rather than aspirational — today nothing could enforce it, because there was nothing to enforce against.

5. **Storage, CRUD, publish, and the till** (slices 2–6). A `ReasonCodeStore` seam over a `store-postgres` table, tenant-scoped by RLS like every other master-data table; `/admin/reason-codes/*` CRUD behind `console.reason_codes.manage`, audited; `PUT /admin/config/reason-codes` composing the node through the same `CapabilityValidator` path every node publish uses; the edge applying it; and on the till, a void control with a reason picker that lists only the codes whose `applies_to` includes the action being taken.

## Consequences

* **The six actions become buildable in any order.** B2.2 (void line, void bill) is the first consumer and the one the roadmap names, but discount, comp, refund and drawer-open each need the same list, and none of them needs another decision after this one. That is the reason this is its own ADR rather than a paragraph inside the void work: five other features were waiting on the same missing thing.

* **A store that never publishes still records honest reasons.** They come from the framework set, and the audit trail says so — the id resolves to a code with a known provenance rather than to nothing.

* **Replace, not merge.** A published node replaces the default set rather than adding to it. Merging would leave an operator unable to remove a framework reason they judge wrong for their business, and "why is this option still there" is a worse failure than "I must list the ones I want".

* **`applies_to` is enforced, not advisory.** An entry that lists no action is rejected at publish: a reason valid for nothing is a data-entry mistake, and accepting it would put an unusable row in a picker.

* **No new domain arithmetic.** `pos_core` gains nothing; `decide_line`'s void branch is already written and already correct. This ADR is entirely about giving it an input it can trust, which is the same thing ADR-0079 did for the inventory engine.

* **What this does not decide.** Whether a reason is required for an *unfired* line void. The spec requires one for voiding a **fired** line (§5) and for a bill void (§6); `decide_line` treats voiding an unfired line as "an ordinary cancel" with no permission and no PIN. Slice 5 will follow the spec — reason required where the spec requires it — and the field stays on the event either way, because the event catalogue is already committed and additive-only.
