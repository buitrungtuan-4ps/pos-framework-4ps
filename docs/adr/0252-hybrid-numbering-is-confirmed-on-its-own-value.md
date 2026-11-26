# ADR-0252 — HYBRID numbering is confirmed on its own value, not by a separate setting

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 · Supersedes in part
[ADR-0251](0251-a-vendor-numbered-invoice-waits-for-its-number-and-the-store-learns-it.md) decision 6
(the setting `einvoice.hybrid_confirmed` only) · Relates to
[ADR-0169](0169-a-setting-value-can-require-a-recorded-confirmation.md),
[ADR-0249](0249-the-e-invoice-number-comes-from-the-source-the-store-chooses.md)

Every statement of law below is **to be confirmed by legal or the accountant**.

## Context

ADR-0251 decision 6 lets the cloud publish `EINVOICE_NUMBER_SOURCE_HYBRID` only when a separate
value, `einvoice.hybrid_confirmed`, records who confirmed that legal and the vendor allow it. The
register already has a mechanism for exactly this: ADR-0169 lets one value of a setting require a
recorded confirmation of a notice, and the cloud writes that value only with it.

## Options considered

1. **A separate `einvoice.hybrid_confirmed` setting**, as ADR-0251 says. Two settings to keep in step,
   and one of them is meaningless unless the other is `HYBRID`.
2. **The ADR-0169 confirmation on the `HYBRID` value itself.** One setting; the console asks for the
   confirmation where the value is chosen, and the cloud records who confirmed it.

## Decision

1. **Option 2.** `einvoice.number_source` carries an ADR-0169 `Confirm` on
   `EINVOICE_NUMBER_SOURCE_HYBRID`, with the notice `settings.item.einvoice.number_source.confirm`:
   hybrid numbering is allowed only if legal and the e-invoice vendor have confirmed it for this
   seller. The cloud writes `HYBRID` only with that confirmation recorded.
2. There is no `einvoice.hybrid_confirmed` setting. Everything else in ADR-0251 decision 6 stands:
   the offline symbol, what "offline" means, and waiting for the vendor without an offline symbol.
3. **Fixed, as before:** `HYBRID` is off by default (`POS` is the default), and no template or
   country pack presets it.

## Consequences accepted

- The same effect as ADR-0251 decision 6 intended (who confirmed is recorded, off by default) with
  one setting fewer.
- A tenant that later loses the vendor's or legal's confirmation switches the store's source back;
  the confirmation is recorded with each write of `HYBRID`, not withdrawn on its own.
