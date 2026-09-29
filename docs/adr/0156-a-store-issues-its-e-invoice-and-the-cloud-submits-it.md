# ADR-0156 — A store issues its e-invoice from a range, and the cloud submits it

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-09-29
· Relates to [ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
[ADR-0152](0152-a-receipt-number-belongs-to-a-series-and-each-country-sets-its-rules.md),
[ADR-0107](0107-the-buyer-is-a-subject.md), [ADR-0027](0027-country-modules.md),
[ADR-0001](0001-offline-first-store-autonomy.md)

## The problem

`docs/architecture.md` §6.1 promises that invoices can be issued offline from pre-allocated ranges,
and that "the store only knows allocated numbers plus a queue". Nothing yet decides who submits the
queue, or where. The `Fiscalization` port puts both halves behind one trait: consuming a number
locally, and talking to the authority's provider. If the edge called it, a tenant's e-invoice
credentials would sit on every till, which ADR-0153 rules out. And no event records that an invoice
was issued at all.

## Options considered

| | Option | Offline issue | Provider credentials | Replay after a lost submission |
|---|---|---|---|---|
| A | The edge issues and submits | Yes | **On every store** | From the edge's outbox |
| B | **The edge issues from its range and writes an event; the cloud submits** | Yes | Cloud only | From the event log |
| C | The cloud issues and submits | **No** | Cloud only | From the event log |

C stops a legal invoice when the internet does. **B is chosen.**

## Decision (proposed)

1. **Issuing is the store's, in the same transaction as the sale.** When a settle asks for an
   invoice, the edge takes the next number from the range the cloud allocated it. The country module
   formats the number and holds the legal rules; `fiscal-vn`, for example, holds the series symbol
   and when it restarts. The edge writes a new `fiscal.invoice.issued` event carrying the bill, the
   series, the number, the totals by tax rate and the buyer's `subject_id`, never the buyer's
   details (ADR-0107). An invoice is issued exactly when its sale commits, or not at all.
2. **Submitting is the cloud's, from the event log.** The cloud's e-invoice connector, the tenant's
   one enabled `einvoice.<vendor>` connection, consumes `fiscal.invoice.issued` and submits the
   invoice. It reads the buyer's details from the subject store and records `submitted`, `rejected`
   or `retrying` against the invoice. Because the queue *is* the event log, a lost submission is
   replayed rather than reconstructed, and a vendor switch submits the rest of the queue to the new
   connection.
3. **Ranges are allocated ahead, per store.** The cloud allocates a store its next range, from the
   provider where the vendor requires that, and publishes it in the store's configuration. An
   alert fires while enough numbers remain to trade offline for the planned outage. A store that
   runs out stops issuing invoices, not selling.
4. **Reconciliation compares both sides daily** — invoices the store issued that the provider does
   not hold, and the reverse — using the port's existing `reconcile`, run by the cloud.
5. **`einvoice.sandbox`** ships in every build. It accepts, rejects or times out on command, so CI
   drives issue → submit → reconcile end to end without a vendor.

## Consequences accepted

- `fiscal.invoice.issued` is a new event, and the range travels in configuration. Both are
  `pos-proto` changes and need the owner's review.
- The `Fiscalization` port's `issue` stops being something a store calls. The port is split, or
  documented as cloud-side only, in the PR that introduces the connector. That PR also needs the
  owner's review.
- An invoice submitted days after its sale, because the store was offline, is legal only within the
  country's deadline. The country module states the deadline, and the alert above respects it.

## Not decided here

Adjustment and replacement invoices; cancellation; which Vietnamese providers to build first; the
receipt series' relationship to the invoice series (ADR-0152).
