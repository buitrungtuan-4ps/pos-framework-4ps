# ADR-0249 — An e-invoice's number comes from the source the tenant, store or vendor connection chooses

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 · Extends
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md) decision 2 and
[ADR-0229](0229-the-e-invoice-issue-moment-is-a-schedule.md) decision 3 · Relates to
[ADR-0240](0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md),
[ADR-0241](0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)

Every statement of law below is **to be confirmed by legal or the accountant**.

## Context

Today the store numbers each invoice at issue from its own symbol, gapless and offline (ADR-0225).
Some vendors allocate the number themselves. The owner wants the source to be a choice.

## Decision

1. **`einvoice.number_source`**, per tenant, store or e-invoice connection:
   - `EINVOICE_NUMBER_SOURCE_POS`, **the default**: today's behaviour.
   - `EINVOICE_NUMBER_SOURCE_VENDOR`: the store issues the invoice without a number
     (`fiscal.invoice.issued` with the number pending, additive); the cloud submits it and records the
     number and lookup code the vendor returns; the edge learns them on its next sync, and the till
     reprints or e-mails them. An offline bill prints "number pending".
   - `EINVOICE_NUMBER_SOURCE_HYBRID`: the store numbers offline from its own symbol and the vendor
     numbers online. **Allowed only if legal and the vendor confirm**: the cloud refuses it unless
     `einvoice.hybrid_confirmed` is set, a value that records who confirmed it (ADR-0169).
2. **Fixed:** numbering stays gapless per symbol; there is no "skip a number" option and no option to
   sell without an invoice beyond what ADR-0229's issue moments allow.
3. **Build order:** the setting with `POS` and `VENDOR` first; `HYBRID` is documented and not built
   until legal and a vendor confirm it.

## Consequences accepted

- An additive event field, a cloud write-back of the vendor's number, and a till reprint. The default
  changes nothing.
- Under `VENDOR` a guest leaving offline gets no number on paper; the e-mail or the lookup page is the
  only way to give it later.
