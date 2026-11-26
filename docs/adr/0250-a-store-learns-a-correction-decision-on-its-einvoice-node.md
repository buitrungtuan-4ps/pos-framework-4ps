# ADR-0250 — A store learns HQ's decision on a correction request on its own `einvoice` node

**Status** Accepted 2026-11-26 under the owner's decisions (ADR-0245 route b) · **Owner**
@maintainers-architecture · **Date** 2026-11-26 · Phase 11 (open item O2) · Completes
[ADR-0245](0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md) decisions 1 and 4 ·
Follows the precedent of [ADR-0240](0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)

Every statement of law below is **to be confirmed by legal or the accountant**.

## Context

Under `INVOICED_CORRECTION_REQUEST` a store manager asks from the till, and HQ or the accountant
decides in a console queue. ADR-0245 does not say how the store learns the decision. The cloud never
dials a store, and the `CloudSync` port has no call for it. A new call is a port change.

## Options considered

1. **A new `CloudSync` call** the edge polls for decisions. A port change, a contract-test change and a
   new cloud route for one bounded list.
2. **The cloud writes the decision onto the store's own `einvoice` node**, as it already writes the
   invoice `symbol` there (ADR-0240), and publishes. The edge already pulls every config version.
3. **The till asks the console directly.** The till has no cloud session; refused.

## Decision

1. **Option 2.** `PublishedEInvoice` gains an additive field `correction_decisions`: a list of at most
   **200** entries, written only on the store's Store layer and never inherited. It is not a setting:
   the console never offers it, and a settings publish leaves it as it is.
2. **Each entry** holds `bill_id`, `decision` (`CORRECTION_DECISION_APPROVED` or
   `CORRECTION_DECISION_DECLINED`, with the `*_UNSPECIFIED` zero value), `decided_by_user_id` (the
   console user's ULID; no name) and `decided_time`. A new decision for a bill replaces its entry. Past
   200 the oldest entry is dropped; a dropped approval is approved again from the queue.
3. **The flow.** The till's request records `fiscal.correction.requested` (ADR-0245 decision 4); the
   cloud's feed keeps it in a queue table. Approve or decline writes the entry, audits it with the
   requester and the approver, and publishes the store's config. The edge, folding a version whose list
   newly holds a bill it requested, records `fiscal.correction.decided` with the entry's fields.
4. **The till honours an approval once.** A bill whose entry is `APPROVED` may be voided by a manager
   at the till; the void is the ordinary `billing.bill.voided`, which the cloud corrects (ADR-0245, #1402).
   So nothing is issued and no money leaves the drawer before approval. A `DECLINED` entry, or none,
   leaves the void refused. A voided bill ignores its entry.
5. **Routing.** Above `einvoice.manager_correction_limit` (`MANAGER_UP_TO_LIMIT`) and after
   `einvoice.cancel_window_minutes` (`CANCEL_WINDOW`) the till offers the same request.
6. **Fixed:** the list is bounded; no personal data in the list, the events or the logs; a reason code
   from `VOID_BILL` is required while `einvoice.correction_reason_required` is on (the default).

## Consequences accepted

- No port change; one additive `pos-proto` field and two additive events.
- The store learns a decision at its next config pull, not instantly, and only while online.
- `INVOICED_CORRECTION_REQUEST` is offered in the console only once the queue, the till form and the
  till's honouring are all built.
