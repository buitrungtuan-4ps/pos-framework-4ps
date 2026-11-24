# ADR-0227 — An Indian invoice is registered through the port, UPI is confirmed, GST returns are exported

**Status** Accepted 2026-11-24 under the owner's instruction to complete phases 7–9 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-24
· Phase 7 (roadmap-v4 row 7, roadmap-v3 B10.2) · Touches tax and a port's use (AGENTS.md §7) ·
Relates to [ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
[ADR-0155](0155-a-qr-payment-is-confirmed-by-its-gateway-through-the-cloud.md),
[ADR-0156](0156-a-store-issues-its-e-invoice-and-the-cloud-submits-it.md),
[ADR-0104](0104-multi-component-and-inclusive-tax.md), ADR-0225

**Open owner decisions**, taken here as recommended: (1) **how deep India goes**: everything below
is built against fakes, because there is no GSP registration; (2) **which GSP and which UPI gateway**:
none is chosen. Every threshold and every statement of Indian law below is **to be confirmed by
legal or the accountant**.

## Context

The `in` pack has GST at 5 % for restaurants split CGST and SGST, tax-inclusive prices, rounding to
the rupee, a GSTIN shape check and a seller-chosen invoice number (Rule 46). Its findings: IGST
cannot be chosen per bill (F1), there is no HSN or SAC code (F2), and alcohol has no national row, so
a store publishes its state's rate (R7). India's e-invoicing registers a B2B invoice with the Invoice
Registration Portal (IRP), which returns an IRN and a signed QR, above an aggregate-turnover threshold
that has fallen several times. A restaurant's B2C sale is generally outside it. UPI is how most
Indian guests pay.

## Decision

1. **IRP registration is a submission behind `Fiscalization`, in the cloud** (ADR-0225 decision 8).
   The store numbers its invoice under Rule 46 from its own series (ADR-0152, restarting on 1 April)
   and writes `fiscal.invoice.issued`, as Vietnam does. The cloud's `einvoice.<gsp>` connection
   registers it and records the IRN and the signed QR against the invoice. The receipt prints at
   settle without them, and a reprint carries them once they are known. Only a bill with a buyer
   GSTIN is registered, and only when the tenant's `einvoice.irp_enabled` is on (a setting, default
   off, because the threshold is the tenant's fact, not the software's). Until a GSP is registered,
   only `einvoice.sandbox` serves, returning a well-formed fake IRN and QR.
2. **UPI is a QR payment confirmed by its gateway** (ADR-0155). The till shows a UPI intent QR for the
   amount (payee VPA from the store profile, the bill's id as the reference). Confirmation is the
   gateway's webhook through the cloud, or the cashier's tap on **Received** when it does not come,
   which ADR-0155 keeps as the offline path. A hand confirmation is marked as such for the day's
   reconciliation. The gateway is the `qr.sandbox` fake until one is chosen.
3. **GSTR export.** The cloud exports a month's settled bills as the GSTR-1 sections a restaurant
   files (B2B by GSTIN, B2C summarised by state and rate, HSN summary, documents issued) to a file
   for the accountant to upload. Filing itself is the accountant's, through their GSP, and nothing is
   sent to GSTN.
4. **HSN and SAC.** An item gains an optional `hsn_sac_code`, additive catalogue data. A restaurant
   service is SAC 9963 by default, which the `in` pack seeds and an item may override (to be
   confirmed). The export and the invoice read it.
5. **Alcohol at state rates.** Alcohol stays outside the pack's table (R7). The store's `tax` node
   carries its state's VAT on liquor as a tax class of its own with a single component named `VAT`,
   so a bill shows GST and VAT lines apart. The `in` pack refuses an alcohol sale with no published
   rate, as it does today.
6. **IGST** (F1): a bill whose buyer's GSTIN names another state takes IGST at the full rate. The
   state is the GSTIN's first two digits, compared with the seller's. Only a bill with a buyer can be
   inter-state.

## Configuration

- **Settings:** `einvoice.irp_enabled`, default off. **Catalogue:** `hsn_sac_code` per item.
  **Store profile:** the UPI payee VPA and the seller's GSTIN.
- **Fixed (law):** CGST plus SGST, or IGST, by place of supply; rounding to the rupee; Rule 46
  numbering restarting on 1 April.

## Consequences accepted

- India works end to end against fakes. The exit record states that no real IRP, UPI gateway or
  GSTN path is verified.
- An additive catalogue field and a setting. The IRP fields on the invoice record are the cloud's.
