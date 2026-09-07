# ADR-0116 — The staff-confirmation hold is derived state, and it gates firing

**Status** Accepted · **Owner** @maintainers-edge · **Last reviewed** 2026-09-07
**Relates to** [ADR-0012](0012-qr-ordering-via-cloud.md) (QR ordering through `POST /v1/orders`; staff confirmation on by default) · [ADR-0057](0057-qr-ordering.md) (the signed table token, the guardrails, and **why there is no per-session token in v1**) · [ADR-0064](0064-edge-order-in.md) (the intake path and the idempotency ledger) · [ADR-0080](0080-channels-and-payments.md) (the `qr` config node that carries the policy) · [ADR-0111](0111-a-second-origin-may-address-the-edge.md) (`docs/snapshots/routes.txt`, so a new route is additive) · `docs/pos-spec.md` §11 item 2 (the reason a rejection cites), §13 (QR ordering) · [`docs/roadmap-v3.md`](../roadmap-v3.md) B2.2's sibling QR work

**Context.** ADR-0012 makes staff confirmation the headline protection on a printed QR code: *"Static printed QR codes are protected by staff confirmation (on by default), per-table rate limits, business-hours-only acceptance, and rejection when the store is offline."* ADR-0057 says the same in the language of the guardrail — `Accept { require_staff_confirmation }`, defaulting to `true` — and `OrderAcceptance::awaiting_staff_confirmation`'s own doc comment says it is *"what stops a passer-by ordering forty pizzas to a table they are not sitting at."*

**None of that was true.** The flag had exactly three readers, and all three only *report* it: the relay response (`pos-cloud/src/relay.rs`), the public intake response (`pos-cloud/src/orders.rs`), and the idempotency ledger row (`IntakeRecord`). Nothing on the fire path consulted it. A guest's order reached the kitchen the moment any member of staff pressed fire, exactly as an order with no hold would. The protection was a field, not a gate.

This also corrects the record on a prior finding. An audit read the four declared QR events as *"four events with no producer"* and the hold as *"a QR order held for staff confirmation can never be released"*. Both readings were wrong in the same direction — they assumed a gate existed:

* Nothing was stuck, because nothing was held. The severity was the opposite of "stranded": it was "unprotected".
* Two of the four events, `sales.qr_session.started` and `sales.order.submitted_by_guest`, carry a `QrSessionId`. ADR-0057 **rejected a per-session token for v1** and named it *"a possible v2 for dynamic on-screen codes"*. They have no v1 producer **by design**, and building one would mean overturning an accepted decision, not closing a gap. This ADR leaves them unproduced and says so where a reader will find it.

The other two, `sales.order.confirmed_by_staff` (`order_id`) and `sales.order.rejected_by_staff` (`order_id`, `reason_code_id`), carry no session. They are buildable today, and they are exactly what a hold needs in order to end.

**Decision.** The hold is **derived from the event log and the live policy**, never stored as a mutable flag, and it **gates firing**.

1. **Derived, in one predicate.** An order is awaiting staff confirmation when all four hold:

   * it opened on the **QR** channel (`sales.order.opened.channel`),
   * it names a **table** (`sales.order.opened.table_id`),
   * the store's **current** `qr` policy requires confirmation (`EdgeSession::qr_staff_confirmation_required`),
   * and no `sales.order.confirmed_by_staff` or `sales.order.rejected_by_staff` has been recorded for it.

   Nothing needs clearing, because nothing was set. Turning the policy off releases every held order at once — the behaviour PR #234 established for the *reported* answer, now the same answer for the *enforced* one, because it is the same predicate. Storing a boolean and clearing it later is the alternative, and it is how the two answers drifted apart in the first place.

2. **The channel is part of the predicate.** `holds_for_staff_confirmation` previously asked only `table_id.is_some() && policy`. On the intake path every tabled inbound order happens to be a QR order, so the answer was right by accident; as a projection query over every order it would have held every dine-in table a server seated by hand. The channel is now an argument, so the reported answer and the enforced answer are one function with one set of inputs.

3. **It gates fire, and only fire.** `POST /api/lines/{id}/fire` refuses a line on a held order with an AIP-193 envelope naming the order. Adding lines, opening a bill, and settling are all left alone: the guest's order is real, it is priced, and it is billable — what staff have not yet agreed to is *making* it. Gating intake instead would mean refusing the order at the door and losing it; gating payment would strand a guest who wants to pay.

4. **A rejection cites a reason, from the list that now exists.** `sales.order.rejected_by_staff.reason_code_id` is mandatory on the event, and `docs/pos-spec.md` §11 item 2 requires it to come from the cloud-managed list. [ADR-0115](0115-reason-codes-are-a-managed-list.md) built that list, and `REASON_ACTION_REJECT_ORDER` is the action it is validated against. Until the `reason_codes` node is authored and published (ADR-0115 slices 2–4), the edge validates against `PublishedReasonCodes::framework_default()` — which is precisely the posture ADR-0115 chose so that a store is never unable to act.

5. **A rejection closes the order.** `sales.order.rejected_by_staff` is followed by `sales.order.closed`: a refused guest order is not a live order waiting on the floor, and leaving it open would put it on the counter list and the table's bill. Rejecting after a line has fired is refused instead — that is a void, with a void's permission and its own reason (roadmap B2.2), not a rejection.

6. **A new permission, `sales.order.confirm_qr`.** Confirming a guest's order releases it to the kitchen and rejecting it ends it, so it is not something any signed-in device should do silently. Risk `Medium`, no PIN — a server standing at the table is the intended actor, and a PIN prompt on every guest order would be abandoned within a shift. `Cashier, Server, Supervisor, Manager, Owner`, matching who may open an order in the first place.

## Consequences

* **The guardrail ADR-0012 and ADR-0057 promised is now enforced.** This is a behaviour change on upgrade for any store already accepting QR orders: with the default policy on, a guest's order must be confirmed before it can be fired. That is the documented intent, and it is in the changelog as an upgrade note.

* **The reported and the enforced answers cannot drift.** One predicate, four inputs, two callers. The regression PR #234 fixed was two callers computing one answer from different snapshots; the fix then was to ask once. The fix now is that there is only one thing to ask.

* **Two events stay deliberately unproduced, and the reason is written down.** `sales.qr_session.started` and `sales.order.submitted_by_guest` wait on the v2 per-session token ADR-0057 declined. They are not a gap; a future ADR that introduces the token is what produces them. `docs/production-readiness.md` records that, so the next audit does not re-raise it.

* **The hold survives a restart without a migration.** It is a projection query over events the log already carries, so a rebuilt edge reaches the same answer with no new table and no new column. The intake ledger's `awaiting_staff_confirmation` stays exactly what it is — the *intake-time* answer a repeat caller is owed — and stops being mistaken for live state.

* **What this does not decide.** Whether a store may configure *who* confirms (a per-role narrowing beyond the permission's default roles), and whether a confirmation should print. Both sit behind the permission registry and the print rail respectively, and neither is needed for the guardrail to work.
