# ADR-0245 — A refund or void of an invoiced bill takes the route the tenant or store chooses

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (open item O2) · Extends
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md) decision 7 and
[ADR-0241](0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md) decision 5 ·
Relates to [ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0229](0229-the-e-invoice-issue-moment-is-a-schedule.md)

Every statement of law below is **to be confirmed by legal or the accountant**.

## Context

A bill with an issued e-invoice cannot be voided at the till (`409`); HQ corrects it in the console
(ADR-0241). In Vietnam every bill is invoiced at once, so every refund means a call to HQ. The owner
wants the route to be a choice per tenant or store.

## Decision

1. **`einvoice.invoiced_correction`**, a published setting at every scope, with four modes:
   - `INVOICED_CORRECTION_HQ_ONLY`, **the default**: the till refuses; HQ corrects in the console
     (today's behaviour).
   - `INVOICED_CORRECTION_REQUEST`: a store manager requests the correction from the till, with the
     amount and the reason; HQ or the accountant approves or declines it in a console queue. Nothing
     is issued until it is approved.
   - `INVOICED_CORRECTION_MANAGER_UP_TO_LIMIT`: a store manager may correct up to
     `einvoice.manager_correction_limit` (minor units, default `0`) per bill; above it, the request goes
     to the queue as in `REQUEST`.
   - `INVOICED_CORRECTION_CANCEL_WINDOW`: while the invoice is not yet submitted to the vendor and is
     younger than `einvoice.cancel_window_minutes` (default `0`), a manager may cancel it at the till;
     after that, a correction as in `REQUEST`. Whether an unsubmitted invoice may be cancelled
     rather than corrected is **to be confirmed by legal or the accountant**.
2. **Each mode also sets** `einvoice.correction_kind` (`EINVOICE_KIND_ADJUSTMENT`, the default, or
   `EINVOICE_KIND_REPLACEMENT`) and `einvoice.correction_reason_required` (default on). Which errors
   need which kind is to be confirmed by legal or the accountant.
3. **Fixed, not settings:** the correction is numbered under the tenant's correction symbol by the
   cloud (ADR-0240 decision 6), never below zero (#1382); the requester and the approver are both
   recorded and audited; the money leaves the drawer only once the correction is approved or within
   the manager's own limit.
4. **Additive events**: `fiscal.correction.requested` (bill, invoice, amount, kind, reason, requester
   as a staff id) and `fiscal.correction.decided` (approved or declined, by whom). No personal data.

## Consequences accepted

- Three settings, two events, a console queue and a till form. The default changes nothing.
- A store in `REQUEST` waits for HQ before the guest's money is returned; the store chooses
  `MANAGER_UP_TO_LIMIT` to avoid that for small amounts.
