# ADR-0251 — A vendor-numbered invoice is issued "number pending" and the store learns its number

**Status** Accepted 2026-11-26 under the owner's decisions; superseded in part by [ADR-0252](0252-hybrid-numbering-is-confirmed-on-its-own-value.md) (the HYBRID confirmation is the ADR-0169 confirmation on the value, not `einvoice.hybrid_confirmed`) · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 · Details
[ADR-0249](0249-the-e-invoice-number-comes-from-the-source-the-store-chooses.md) and supersedes its
decision 3 (build order) · Follows the precedent of
[ADR-0250](0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md) · Relates to
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
[ADR-0229](0229-the-e-invoice-issue-moment-is-a-schedule.md),
[ADR-0240](0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md),
[ADR-0242](0242-a-receipt-prints-its-invoice-number-and-lookup-code.md)

Every statement of law below is **to be confirmed by legal or the accountant**.

## Context

ADR-0249 makes the number source a setting but leaves open what the event carries without a number,
how the cloud asks the vendor for one, how the store learns it, and how a change of source keeps each
symbol gapless. The owner also asked for `HYBRID` to be built behind a confirmation that is off by
default, where ADR-0249 decision 3 had left it unbuilt.

## Options considered

1. **A new `Fiscalization` call that asks for a number.** Not needed: `issue` with no
   `issued_number` already lets the provider take its own (port contract 5 reads only when a number
   is given). A port change for nothing.
2. **The edge polls the cloud for numbers.** A `CloudSync` port change for one bounded list; refused,
   as in ADR-0250.
3. **The cloud writes each number onto the store's own `einvoice` node**, as it writes the symbol and
   the correction decisions, and the edge records it. No port change. Chosen.

## Decision

1. **The event.** `FiscalInvoice` (in `fiscal.invoice.issued`) gains the additive field
   `number_source` (`EINVOICE_NUMBER_SOURCE_POS`, `_VENDOR`, `_HYBRID`, with the `*_UNSPECIFIED`
   zero value; absent reads as `POS`). An invoice issued under `VENDOR` carries `invoice_sequence: 0`,
   which no store-numbered invoice holds (sequences start at 1), and the symbol it is to be numbered
   under. It is issued at the same moment as any other (ADR-0229), so nothing sells without an
   invoice record.
2. **The store.** Under `VENDOR` the settle and the deferred issue take no number from the symbol,
   so the store's own sequence never moves and never skips. The receipt prints **"Number pending"**
   in place of the number, and the lookup code as today (ADR-0242), which the store has offline.
3. **The cloud.** The feed records a `VENDOR` invoice with no number and submits it with
   `issued_number: None`; the provider's answer is its number and its own reference. A provider
   that refuses is handled as any refusal (ADR-0241). A correction of a vendor-numbered invoice waits
   until its number is known.
4. **The store learns the number.** `PublishedEInvoice` gains `vendor_numbers`: at most **200**
   entries of `bill_id`, `invoice_number` and `authority_reference`, written only on the store's
   Store layer, never inherited and never offered as a setting; the oldest is dropped past 200. The
   cloud writes the entry and publishes when the provider answers. The edge, folding a version that
   newly holds a bill whose invoice is pending, records **`fiscal.invoice.numbered`** (additive) once
   per bill. A reprint of the receipt then shows the number. Where the buyer gave an e-mail, the
   provider e-mails the invoice as it does for any invoice; the store sends nothing itself.
5. **A change of source keeps each symbol gapless.** The source is read per invoice at issue. The
   edge folds each learned vendor number into its symbol's last number. A store switched from
   `VENDOR` back to `POS` keeps issuing "number pending" while any of its invoices still waits for a
   number, so it never numbers over a number the vendor is about to give.
6. **`HYBRID`** numbers offline from a second symbol, `offline_symbol`, which the cloud allocates
   from the same ledger as every symbol (ADR-0240), and asks the vendor online. "Offline" is the
   edge's own view: its last delivery to the cloud failed or is older than two minutes. Each symbol
   has one source, so each stays gapless. The cloud refuses to publish `HYBRID` unless
   `einvoice.hybrid_confirmed` is set, a value that records who confirmed (ADR-0169), labelled
   **"allowed only if legal and the vendor confirm"** and off by default. Without an
   `offline_symbol` an offline `HYBRID` store issues "number pending" as under `VENDOR`.
7. **Fixed:** numbering stays gapless per symbol; there is no "skip a number" option and no sale
   without an invoice beyond ADR-0229's issue moments; `POS` stays the default; no personal data in
   the list, the events or the logs.
8. **Offered in the console** only once each part is built: `VENDOR` with decisions 1 to 5, `HYBRID`
   with decision 6.

## Consequences accepted

- No port change; additive `pos-proto` fields and one additive event.
- A guest who leaves while the store is offline under `VENDOR` gets no number on paper, only the
  lookup code; the number reaches them by the provider's e-mail or a reprint.
- A store learns a number at its next config pull, so only while online.
