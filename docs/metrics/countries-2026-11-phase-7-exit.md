# Countries at the end of phase 7, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-24

This is the exit record of roadmap-v4 phase 7 (countries). It records what the exit criterion asked,
how far the criterion is met, the decisions taken, and what is still open. Like
[templates-2026-11-phase-6-exit.md](templates-2026-11-phase-6-exit.md), it records one moment and is
not regenerated.

**Measured at** `78044e04` on `main`, with the open pull requests named below applied.

## The criterion

**Each country's legal acceptance suite passes.** A country's suite is everything its
`countries/<cc>/CONFORMANCE.md` maps:

- `tests/conformance.rs`: golden bills computed by `pos_core::billing::assemble`;
- `tests/contract.rs`: the `Fiscalization` contract suite;
- for Vietnam, also `tests/einvoice.rs` and `tests/issue_schedule.rs`.

A suite passes when `cargo test` on its crate passes. Passing proves that the code does what each row
says. **It does not prove the law.** Every row marked *to confirm* waits on legal or the accountant.

## Where the criterion stands: partial. Met for Vietnam and Japan, not yet for India

| Country | Suite | Passes | What it covers | What it cannot cover yet |
|---|---|---|---|---|
| Vietnam (the pilot) | cases 1–14, R1–R24 | **Yes** | VAT and rounding; the MST shape; the invoice symbol, with a gapless numbering per issuing point that restarts with the year; original, adjustment and replacement invoices; the buyer check; totals per rate; comp tax; no scale-label layout; hand weighing forbidden; the five issue modes and the end-of-day legal limit | The field set a provider submits (R11), until a provider is chosen |
| Japan | cases 1–11 | **Yes** | 8 % and 10 % rates; tax extracted from 税込 prices; no cash rounding; the shape of the 登録番号; **tax rounded once per rate per invoice**; comp tax; no scale-label layout; hand weighing forbidden | The qualified invoice's printed fields and its `ja` labels (R12) |
| India | cases 1–10, R1–R21 | **Partly** | GST at 5 % printed as CGST and SGST; tax extracted from MRP; rounding to the rupee; Rule 46 numbering; the GSTIN shape; alcohol refused without a state rate; comp tax; no scale-label layout; hand weighing forbidden; the place of supply; the GSTR-1 section; the HSN and SAC shape; the UPI request | ADR-0227's IRP registration, UPI confirmation, GSTR-1 export, HSN or SAC per item and IGST on the bill are decided but **not built** |

India's suite passes, but it does not yet exercise ADR-0227's core decisions. **The criterion is not
yet met for India.**

## What shipped

| Record or slice | What shipped | Pull requests |
|---|---|---|
| [ADR-0225](../adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md) Vietnamese e-invoice | The store numbers its e-invoice in the sale, and the cloud submits it. ADR-0156's option B, settled for Vietnam | #1340 |
| [ADR-0226](../adr/0226-a-japanese-receipt-is-a-qualified-invoice.md), [0227](../adr/0227-an-indian-invoice-is-registered-through-a-port-and-upi-is-confirmed.md), [0228](../adr/0228-comp-tax-scale-labels-and-hand-weighing-stay-conservative-per-country.md) | Japan's qualified invoice; India's IRP, UPI and GSTR; the phase 6 carry-overs kept conservative | #1341 |
| [ADR-0229](../adr/0229-the-e-invoice-issue-moment-is-a-schedule.md) issue schedule | The owner's issue schedule, the legal limit, and the rule that a deferred invoice is numbered when it is issued | #1348 |
| Vietnam: invoice model and series | Symbol, gapless sequence, kinds, buyer check and totals check, in `pos-country-vn::einvoice` | #1342 |
| Vietnam: the port and the sandbox | `InvoiceRequest` carries the store's own number and what a correction corrects. Two new contract cases. `einvoice.sandbox` in the cloud catalogue | #1345 |
| Vietnam: cloud submission and retry | A bounded submission queue in the cloud with retry, refusal and overdue handling | #1350 |
| Vietnam: acceptance suite | Cases 8 to 14, and `comp_failure` | #1349 |
| Vietnam: issued at the settle, on a schedule | The `einvoice` node and its three settings. `fiscal.invoice.issued` and `fiscal.invoice.scheduled`. The edge's issue loop. The legal limit, clamped on the edge and refused by the cloud. Each invoice names its connection. `E_INVOICE_SYMBOL_MISSING`, and `E_INVOICE_ISSUED` now in force. The VN schedule cases | #1355 |
| Japan: tax once per rate, and the JP/IN carry-overs | `TaxRoundingScope`, and `billing/per_rate.rs`. Japan's case 6 comes to ¥183. Japan's cases 9–11 and India's cases 8–10 | #1361 |
| Vietnam: a store's symbol | HQ gives one store its e-invoice symbol with `PUT /admin/config/einvoice-symbol`. The route checks the symbol against the store's country and is audited | #1360 |
| India: GST facts | Place of supply, GSTR-1 section, HSN and SAC shape, and the UPI intent, in `pos-country-in::gst` | #1359 |

## Pilot blocker: how a symbol reaches a store

**A Vietnamese store cannot settle until its invoice symbol is published.** The Vietnamese pack
seeds `EINVOICE_ISSUE_MOMENT_IMMEDIATE` for every new store. A store that must issue an invoice
refuses to settle, with `409 E_INVOICE_SYMBOL_MISSING`, until the cloud writes a symbol on its
`einvoice` node (ADR-0225 decision 2). This is the safe behaviour, because an invoice cannot be
numbered later.

**An HQ route exists; automatic allocation does not.** HQ gives each pilot store its symbol with
`PUT /admin/config/einvoice-symbol` (#1360). Nothing yet allocates a symbol per lease generation by
itself, so:

- each pilot store's symbol is set by hand before it trades; and
- a replacement box needs a new symbol set by hand before it settles.

The console has no screen for this route yet.

## Decisions taken

**By the owner on 2026-11-24** (ADR-0229):

- **Vietnam goes first**, then the other countries.
- **Many e-invoice vendors run through adapters.** A tenant, brand or store chooses its provider
  through the catalogue. Several may be live at once. A switch of provider applies only to invoices
  issued after it, because each invoice names its connection.
- **The issue moment is flexible configuration**, with five modes. Vietnam seeds `IMMEDIATE`.

**Within the accepted records:**

- **A deferred invoice takes its number when it is issued, not at settle** (ADR-0229 decision 3).
  A number reserved at settle is orphaned if the bill is voided before issue. Numbers taken at issue
  ascend with the invoices' own dates.
- **Each issuing point has its own symbol** (ADR-0225 decision 2). A replacement box therefore
  never repeats a number, even one issued offline and never synced.
- **The legal limit is checked twice, as law.** Vietnam's limit is the end of the sale's day. The
  cloud refuses an `einvoice.delay_minutes` of 1,440 or more (`SETTING_BEYOND_COUNTRY_LIMIT`), and
  the edge clamps every due time to the limit.
- **A store that must issue an invoice and has no symbol refuses to settle**, before it takes a
  receipt number. The simulator's Vietnamese stores are given a symbol, as the cloud will give one.
- **A paid bill with an invoice is corrected in the console, not voided at the store.**
  `E_INVOICE_ISSUED`, which the core already refused on, now reads the invoices the store issued.
  The console's correction is not built (see below).
- **Tax rounding scope is the country's law, read on the edge from the country it was built for.**
  It is not a setting (ADR-0226).
- **Phase 6's carry-overs stay conservative** (ADR-0228). In `vn`, `jp` and `in`, a comp lowers the
  taxable base, no scale-label layout is seeded, and hand weighing is forbidden.

## Roadmap-v3 B4 checked against the code

- **Multi-component tax**: built (ADR-0104, ADR-0168). India's cases print CGST and SGST that add
  up to the tax.
- **Inclusive rounding**: built. Japan's and India's cases extract tax from inclusive prices. Japan
  now rounds once per rate.
- **HSN and SAC**: only the shape is checked (`gst::is_valid_hsn_sac`). **No item field carries a
  code**, which is a real gap and is ADR-0227 decision 4.
- **Tax-id validation**: Vietnam's MST, Japan's 登録番号 and India's GSTIN are checked for **shape
  only**. No check digit is verified, and a registration is never checked against an authority.
  Adding a check digit before legal confirms the algorithm could refuse a real customer's code at
  the till, so it was left out deliberately.

## Open owner decisions and confirmations

- **Which e-invoice vendors to build first.** The vendor list is open. Until it is chosen, only
  `einvoice.sandbox` serves, and **nothing reaches a tax authority**.
- **The issue moment is decided** (ADR-0229). The legal limit behind it is not: Vietnam's end of
  day is the conservative reading, to be confirmed by legal.
- **How deep to go in India.** Everything in ADR-0227 is to be built against fakes, because there
  is no GSP registration. The IRP, UPI, GSTR-1 export, HSN or SAC field and IGST slices are still to
  come.
- **Where the cloud is hosted.** If it is hosted outside Vietnam, holding a buyer's details for
  submission is a cross-border transfer under PDPD (Decree 13/2023). That transfer needs the
  impact-assessment dossier (ADR-0225 decision 4).
- **Confirmations owed by legal or the accountant**, as each country's `CONFORMANCE.md` lists:
  - Vietnam: the invoice symbol letters, whether several symbols may run at once, the gapless rule,
    the issue modes and the limit, the correction kinds, and the buyer retention period (which must
    follow the invoice's retention, not the 365-day default);
  - Japan: tax once per rate and the rounding mode, and the qualified invoice's fields;
  - India: the B2CL threshold, the HSN and SAC digits by turnover, the e-invoicing threshold, and the
    UPI format;
  - all three: comp tax, GS1 label layouts and hand weighing.

## What is still unverified against a real sandbox

- **No real e-invoice provider.** Vietnamese submission is proved only against `einvoice.sandbox`,
  through the shared contract suite and the submission queue. No vendor's sandbox has been called.
- **The submitter is not wired.** The queue is not yet fed from `fiscal.invoice.issued`, keeps no
  state in PostgreSQL, and raises no overdue alert through `AlertChannel`.
- **The cloud does not yet allocate symbols.** HQ writes each store's symbol through `PUT
  /admin/config/einvoice-symbol` (#1360). Nothing gives each lease generation its own symbol
  automatically, and the console has no screen for the route.
- **Not built in the console**: the invoice list, the submission status, and correction by
  adjustment or replacement (`fiscal.invoice.correct`).
- **Not built in the till**: the buyer form, and the invoice number on the receipt.
- **India**: no IRP, GSP, UPI gateway or GSTN path has been called. All are fakes or not yet built.
- **Japan**: nothing is submitted to an authority, as the law intends. The `ja` receipt labels are
  not yet in the till's catalogue.
