# ADR-0106 — A receipt names who sold, and the store's identity is data

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Builds on [ADR-0100](0100-receipt-and-ticket-printing.md), [ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0105](0105-a-country-pack-is-values.md) · Relates to [`pos-spec.md`](../pos-spec.md) §12,
[ADR-0025](0025-receipt-number-authority.md)

## The problem

The receipt this framework prints is three lines: an optional header, `#41`, and a total.

The header is `None` at every call site, and the comment says why — *no config node carries a store's
display name*. So the paper a guest walks out with does not say which shop sold them anything.

That is bad in Vietnam. It is disqualifying in Japan and India:

- A Japanese **qualified invoice** (適格請求書) must carry the seller's name, the seller's
  registration number (登録番号), the date, what was sold, and the totals **per tax rate** — 8 % and
  10 % separately. Without the registration number the buyer cannot claim input tax, which is the
  entire point of the document.
- An Indian **tax invoice** under Rule 46 must carry the supplier's name and address, the supplier's
  GSTIN, a serial number unique in the financial year, and the tax **broken into CGST and SGST**
  (or IGST).

[ADR-0104](0104-multi-component-and-inclusive-tax.md) made the tax half expressible and
[ADR-0105](0105-a-country-pack-is-values.md) made the country half suppliable. Neither reaches paper,
because the printer is handed a number and a total and nothing else.

## The decision

**Two changes: the store gets an identity node, and the receipt becomes a document.**

### 1. `store_profile`, a node beside `locale`

```rust
StoreProfile {
    legal_name: String,          // the registered name; what the law wants on the paper
    trading_name: Option<String>,// the sign over the door, when it differs
    address_lines: Vec<String>,  // the registered address, as it is written locally
    tax_registration_number: Option<String>,  // 登録番号 / GSTIN / mã số thuế
    tax_registration_label: Option<String>,   // what the paper calls it
    contact_lines: Vec<String>,  // phone, e-mail — what a guest calls about a bill
    footer_lines: Vec<String>,   // the thank-you, the return policy, the QR caption
}
```

**A new node rather than more fields on `locale`**, because the two are different facts with
different authors and different cadences. `locale` is *how this store writes and taxes*: operations
change it when a rate changes. `store_profile` is *who this store legally is*: finance changes it
when a company is renamed or a registration issues. Bolting a registration number onto the node that
carries the cutoff hour would mean a rate change and a legal-identity change are the same publish,
reviewed by the same person, rolled back together.

**Every field is text the operator supplies, and the framework validates almost none of it.** A
registered address is written differently in every country and a framework that imposed a shape on
it would be wrong in most of them. The one thing that *is* checked is the tax registration number,
against the country module's `is_valid_tax_code` — format only, never registration, which is the line
[ADR-0027](0027-country-modules.md) drew and the reason a cashier can take a corporate customer's
number with the line down.

**The label is data too.** Japan prints 登録番号, India prints GSTIN, Vietnam prints MST. The country
module knows which, so the console offers it; the node stores what the operator accepted. A closed
enum here would make the fourth country a code change, which is the thing country packs exist to
avoid.

### 2. The receipt becomes a document

`receipt_document` stops taking a total and starts taking the profile, the bill's totals, and its
lines. It prints, in order:

```
        <trading or legal name>          ← emphasised, centred
        <address lines>
        <registration label>: <number>   ← omitted entirely when unset
        ------------------------------
        <business date>   #<receipt no>
        ------------------------------
        2 x Margherita          198,000
        ...
        Subtotal                900,000
        Service charge           45,000
        VAT 10%                  94,500  ← one line per rate…
          CGST 2.50%             22,500  ← …and its parts, when a country needs them
          SGST 2.50%             22,500
        Rounding                    500
        TOTAL                 1,040,000  ← double size
        ------------------------------
        <footer lines>
```

Three properties, and each is a decision rather than a layout:

- **Every block is omitted when it has nothing to say.** No registration number, no registration
  line. No service charge, no service-charge line. No components, no indented parts. A receipt for a
  Vietnamese store looks exactly as it always did plus a name and a total that reconciles, and a
  blank line is never printed where a value is missing — an empty label on a legal document reads as
  a value somebody forgot, and this way the paper cannot lie by omission.
- **The tax section is per rate, not per line.** That is what both Japan and India ask for, it is
  what `BillTotals::tax_lines` already computes, and it keeps a long bill's receipt short.
- **Under the inclusive posture the total does not move.** ADR-0104's netting means the printed
  subtotal reads net and `subtotal − discount − comps + service_charge + tax + rounding = total_due`
  holds on the paper, in both postures, which is what makes the document check out when somebody adds
  it up.

## What this deliberately does not do

- **It does not put the buyer on the receipt.** A B2B tax invoice carries the *buyer's* name and tax
  code — a Japanese corporate customer's 登録番号, an Indian buyer's GSTIN. That is a fact about
  **this bill**, entered at the till by a cashier the guest has just handed a card to, so it needs a
  field on the bill and an event that records it, not a config node. It is the next slice and it is
  named here so its absence is a decision rather than an oversight.
- **It does not issue an invoice number.** The number on this document is the store's gapless receipt
  number, and conflating it with a legal invoice number is forbidden
  ([ADR-0025](0025-receipt-number-authority.md)). A country whose law wants an allocated number gets
  it from `Fiscalization` and prints it beside this one.
- **It does not template.** No layout language, no per-country receipt file. The blocks above are the
  same everywhere and each is omitted when empty; what differs between countries is *the values*,
  which is the whole argument of ADR-0105. A country that genuinely needs a different **order** of
  blocks is the moment to reconsider, and it has not arrived.
- **It does not print a logo.** An image block would be a second rendering path beside ADR-0102's,
  for something no law asks for.

## Consequences

- A guest's receipt says which shop sold them the food, which is a defect this closes rather than a
  feature it adds.
- A Japanese store can print a qualified invoice and an Indian store a Rule 46 tax invoice, given a
  registration number somebody has been issued — which is the legal step the operator takes, and the
  only step left.
- `BillView` carries the settled bill's `BillTotals` so the printer can read them. That is edge-local
  and crosses no wire.
- The store's identity is one more thing a fork supplies rather than writes.
