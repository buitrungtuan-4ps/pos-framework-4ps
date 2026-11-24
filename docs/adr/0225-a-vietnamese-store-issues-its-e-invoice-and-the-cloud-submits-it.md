# ADR-0225 — A Vietnamese store numbers its e-invoice in the sale, and the cloud submits it

**Status** Accepted 2026-11-24 under the owner's instruction to complete phases 7–9 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-24
· Phase 7 (roadmap-v4 row 7) · Changes a port and the event schema, and touches tax, invoice
numbering and personal data (AGENTS.md §7) · Settles for Vietnam what
[ADR-0156](0156-a-store-issues-its-e-invoice-and-the-cloud-submits-it.md) left open, and takes its
option B · Relates to [ADR-0152](0152-a-receipt-number-belongs-to-a-series-and-each-country-sets-its-rules.md),
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
[ADR-0162](0162-a-bill-is-taxed-at-the-rates-of-one-stated-moment.md),
[ADR-0184](0184-a-settled-bill-is-voided-whole-not-refunded.md),
[ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
[ADR-0074](0074-localization-and-tax.md)

**Open owner decisions**, taken here as recommended and to be confirmed: (1) **which provider**
submits Vietnamese e-invoices: none is chosen, and the sandbox provider stands in; (2) **when an
invoice is issued**: a setting whose default is every sale; (3) where the cloud that holds buyer
details is hosted, which decides whether submission is a cross-border transfer. Every statement of
Vietnamese law below is **to be confirmed by legal or the accountant**; none is settled by this
record.

## Context

The pilot is in Vietnam, where a restaurant's sale needs an e-invoice (hóa đơn điện tử) under
Decree 123/2020 as amended by Decree 70/2025 (`docs/roadmap.md` A2). ADR-0156 proposed that the
store issues and the cloud submits, and left open adjustment, replacement, the provider and the
series. The tree has the pieces and no path: the `Fiscalization` port, an offline allocator
(`pos_country::offline`) that writes a Vietnamese-shaped number, a buyer captured at settle into the
subject store (ADR-0107), the shape check for a mã số thuế (`is_valid_mst`), and a settled-bill void
that already refuses `E_INVOICE_ISSUED`. No event records an invoice, and nothing submits one.

## Options considered

| | Option | Offline | Credentials | Cost |
|---|---|---|---|---|
| A | The edge calls the provider | Breaks when the line drops | On every till | ADR-0153 rules it out |
| B | **The edge numbers the invoice in the sale's transaction; the cloud submits from the log** | Yes | Cloud only | One event, one port change |
| C | The cloud numbers and submits | No invoice offline | Cloud only | A sale without its invoice |

**B is chosen**, as ADR-0156 proposed.

## Decision

1. **The invoice model is the country's** (`pos-country-vn`, ADR-0027). An invoice has a **kind**:
   `ORIGINAL`, `ADJUSTMENT` (điều chỉnh) or `REPLACEMENT` (thay thế). An adjustment or a replacement
   names the invoice it corrects. Its lines carry their tax rate, and its totals are given per rate.
2. **Series and numbering.** A number is the **form digit and six-character symbol** (ký hiệu mẫu
   số, ký hiệu hóa đơn, such as `1C26MAA`) and a **sequence of at most eight digits**, which starts
   at 1 for each symbol. The symbol's two digits are the year, so a new year is a new symbol and the
   sequence restarts. The pack checks the symbol's **shape only**; which letters a restaurant's
   cash-register invoices use is to be confirmed.
   - **Each issuing point has its own symbol.** The cloud gives each store's lease generation
     (ADR-0049) its own two seller-chosen characters, and publishes the symbol in the store's
     configuration. Within its symbol, the store's sequence is gapless and never reused, and it is
     taken in the sale's own transaction, as the receipt number is (ADR-0152 option C). A
     replacement box gets a new symbol and never repeats a number, even one issued offline and never
     synced. That several symbols may run at once, and that a symbol may stop mid-year, are to be
     confirmed.
   - The receipt number stays separate and unchanged (`docs/pos-spec.md` §5).
3. **The issue moment is a published setting** (ADR-0160): `einvoice.issue_moment`.
   - `EINVOICE_ISSUE_MOMENT_OFF`: no invoice. This is the register's default, so a country without
     e-invoicing is unchanged.
   - `EINVOICE_ISSUE_MOMENT_EVERY_SALE`: every settled bill issues an invoice, with a buyer or
     without. This is the **recommended default the Vietnamese pack seeds** (ADR-0183). It is the
     reading of Decree 70/2025's cash-register invoices that is safest to confirm.
   - `EINVOICE_ISSUE_MOMENT_ON_REQUEST`: only a bill whose guest gives buyer details.
   The invoice date is the settle's **calendar** date (the `Fiscalization` port's rule), and its
   rates are the bill's, as ADR-0162 fixes them. Vietnam's pack keeps `tax_point = SETTLED`. Both
   points are to be confirmed by the accountant.
4. **Buyer details are personal data, kept apart.** A name, an address, an email and an
   individual's tax code go to the store's subject store, written in the settle's transaction
   (ADR-0107). The event carries only the `subject_id`. A mã số thuế is checked for shape at the
   till and again at the edge (10 digits, or 10 plus a 3-digit branch). Whether it is registered is
   checked only by the cloud, through the provider, after the sale; a mismatch becomes a correction,
   never a refused sale.
   - **Retention:** a buyer named on an issued invoice is kept for the invoice's own retention
     period, which legal must state, and not for the pack's 365-day default. Until legal answers, the
     cloud never masks a buyer whose invoice is less than a year old. The record says why it is
     held, which is a legal obligation, not consent. Both points are to be confirmed under PDPD
     (Decree 13/2023).
   - **Cross-border transfer:** submission sends the buyer's details from the cloud to a Vietnamese
     provider. If the cloud is hosted outside Vietnam, holding those details is a cross-border
     transfer, and it needs the impact-assessment dossier Decree 13/2023 asks for. This is flagged
     for legal and for the hosting decision.
5. **A new event, `fiscal.invoice.issued`** (additive, version 1), written in the settle's
   transaction. It carries the invoice's id (a ULID), the bill, the kind, the symbol, the sequence,
   the calendar date, the totals per rate, the total, the buyer's `subject_id`, and for a correction
   the invoice it corrects. It holds no personal data. The settled bill's response and its receipt
   carry the number.
6. **Offline issue, later submission.** The store issues with or without the internet. The event
   travels in the outbox as every event does. The cloud's submitter consumes it and submits through
   the tenant's one `einvoice.<vendor>` connection (ADR-0153). It records `PENDING`, `SUBMITTED`,
   `REJECTED` or `RETRYING` against the invoice:
   - a timeout or an unreachable provider is retried with backoff, idempotent by invoice id;
   - a refusal stops and alerts, and is corrected (decision 7), never resubmitted unchanged;
   - an alert fires before the submission deadline, which is to be confirmed. A store that never
     comes back is a known gap, and its invoices are submitted when its log is recovered.
7. **Adjustment and replacement.** A settled bill with an issued invoice is not voided at the store:
   the refusal `E_INVOICE_ISSUED` (ADR-0184) stays. The correction is made in the console by
   somebody who holds `fiscal.invoice.correct` (new, default OWNER and ADMIN). It is recorded in the
   cloud and submitted, as a **replacement** (a wrong buyer, a wrong line) or an **adjustment** (an
   amount up or down, including a whole refund). It takes its number from the **tenant's correction
   symbol**, which only the cloud issues from, so it can never collide with a store's sequence.
   Which errors need which kind, and whether a correction must also be notified to the authority,
   are to be confirmed.
8. **The port is cloud-side.** `Fiscalization` keeps its four operations. The store no longer calls
   `issue`: the connector does, as a submission. `InvoiceRequest` gains, additively, the number the
   store already issued, the kind, and the corrected invoice. A provider that numbers for itself
   ignores none of them; it refuses a request whose number it cannot accept. `einvoice.sandbox`, a
   deterministic fake that accepts, refuses or times out on command, ships in every build and passes
   the shared contract suite.

## Configuration

- **Setting:** `einvoice.issue_moment` (decision 3). Default `OFF`, seeded `EVERY_SALE` by the
  Vietnamese pack. The console offers it once the edge reads it.
- **Published per store:** the symbol for its lease generation. **Per tenant:** the correction
  symbol, and the one `einvoice` connection, with its secrets only in the cloud.
- **Fixed (law and money):** the gapless sequence within a symbol; one invoice per bill; the
  calendar date; the per-rate totals; no buyer data in an event or a log.

## Delivery, in slices

The invoice model and series, in `pos-country-vn` · the port change, the sandbox and its contract
tests · the cloud submitter and its retry · buyer capture with the MST check, kept apart · the issue
at settle, with the event and the setting · correction in the console · the till's buyer form and
the invoice number on the receipt · the Vietnamese acceptance suite in `countries/vn`.

## Consequences accepted

- A new event and a port change, both additive. `docs/snapshots/events.txt` grows.
- Selling never waits on the provider. A rejected or late invoice is an alert and a correction,
  never a refused sale. The one exception is a store with no symbol published: it **refuses to
  settle** while `EVERY_SALE` is on, because an invoice cannot be issued later without its number.
- A real provider is one adapter crate and one registry line. Until the owner chooses one, nothing
  reaches the tax authority, and the exit record says so.
