# ADR-0242 — A receipt prints its e-invoice number and a lookup code, and the till asks for the buyer's email

**Status** Accepted 2026-11-25 under the owner's instruction to complete phase 10 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-25 · Phase 10
(roadmap-v4 row 10) · Builds
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md) decisions 4 and
5 · Relates to [ADR-0107](0107-the-buyer-is-a-subject.md),
[ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
[ADR-0207](0207-a-receipt-is-a-structured-template-per-store-and-channel.md)

## Context

ADR-0225 decision 5 says the settled bill's response and its receipt carry the invoice number. Neither
does yet. A Vietnamese guest also needs a way to find the invoice later: providers give a lookup code
(mã tra cứu) that the guest types on the provider's page. A store issues offline, so it cannot wait for
the provider's code. The till's buyer form (ADR-0107) takes a name, a tax code and an address, but not
the email a provider sends the invoice to.

## Decision

1. **The settle answers with the invoice.** The settled bill's response gains, additively,
   `invoice_number` (`1C26MAA/00000042`) and `invoice_lookup_code` when the settle issued an invoice.
   A deferred invoice answers neither; its number exists only once it is issued (ADR-0229 decision 3).
2. **The lookup code is the store's.** It is the last ten characters of the invoice's id (a ULID's
   random part), so the store has it offline, it reveals nothing about the bill, and the cloud derives
   the same code from the id it submits. **Whether a provider accepts a code the seller chose,
   and the page a guest types it on, are to be confirmed with the vendor and legal.**
3. **The receipt prints both**, under the totals, when the bill has an issued invoice. The receipt
   template cannot turn them off for Vietnam (ADR-0207 decision 3), as the law asks the number to be
   given to the buyer; to be confirmed by legal.
4. **The till's buyer form takes an email**, optional, beside the name, tax code and address. It is
   personal data and goes where the others go: the store's subject store, written in the settle's
   transaction, never in an event or a log (ADR-0107, ADR-0194). Its retention is the buyer's
   (ADR-0225 decision 4). The MST is checked for shape in the till before the request is sent.

## Consequences accepted

- Two optional fields on an edge route's response. The edge API snapshot grows.
- A guest who loses the paper loses the code; the invoice is still found by its number.
- No cross-border transfer is added: the email stays on the store until the cloud's submission, which
  ADR-0225 decision 4 already flags under Decree 13/2023.
