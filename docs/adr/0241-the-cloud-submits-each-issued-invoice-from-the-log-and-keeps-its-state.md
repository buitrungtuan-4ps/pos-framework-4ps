# ADR-0241 — The cloud submits each issued invoice from the log, keeps its state, and warns before the limit

**Status** Accepted 2026-11-25 under the owner's instruction to complete phase 10 with the
recommended options · **Owner** @maintainers-architecture · **Date** 2026-11-25 · Phase 10
(roadmap-v4 row 10) · Builds
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md) decisions 6
and 7 · Relates to [ADR-0032](0032-webhooks.md), [ADR-0073](0073-alerting.md),
[ADR-0229](0229-the-e-invoice-issue-moment-is-a-schedule.md),
[ADR-0240](0240-the-cloud-allocates-each-issuing-points-invoice-symbol.md)

## Context

Phase 7 built the cloud's submission queue (#1350) in memory, fed by nothing. An invoice a store
issues reaches the cloud as `fiscal.invoice.issued` and stops there. ADR-0225 decision 6 asks for
`PENDING`, `SUBMITTED`, `REJECTED` and `RETRYING` per invoice, a retry that is idempotent by invoice,
and an alert before the submission deadline. A queue in memory forgets all of it on a restart.

## Options considered

1. **Submit at ingest.** Ingest would wait on the provider, and an outage of the provider would hold
   up a store's whole log. Refused.
2. **A NATS consumer of its own.** A second durable cursor on the broker, for one event type.
3. **A task that reads the event log after a stored cursor**, as webhook delivery does (ADR-0032),
   and keeps each invoice's state in a table.

## Decision

**Option 3.**

1. **Feed.** One cloud task reads the event log in pages after a cursor it stores, takes each
   `fiscal.invoice.issued`, and records it once in `einvoice_submissions` (keyed by the invoice's
   id; a redelivered event changes nothing). It keeps no buyer details: the row holds the `subject_id`
   the event holds, and nothing personal.
2. **State.** Each row holds the state (`PENDING`, `RETRYING`, `SUBMITTED`, `REJECTED`), the failed
   attempts, the next attempt, the provider's reference and the provider's reason for a refusal. The
   retry rule and its bounds are #1350's: one minute doubling to an hour; a refusal is never resubmitted
   unchanged. Each pass submits at most a bounded page of due rows (AGENTS.md §2).
3. **Provider.** Each invoice is submitted through the connection it names (ADR-0229 decision 4). Until
   the owner chooses a vendor only `einvoice.sandbox` serves, so **nothing reaches the tax authority**.
4. **Overdue alert.** A new alert kind, `einvoice_overdue` (Critical), fires per store while any of its
   invoices is not `SUBMITTED` and is within the warning window of its deadline or past it. The deadline
   is the end of the day after the invoice's date and the window is six hours; **both figures are the
   recommended reading, to be confirmed by legal or the accountant**. It clears once every such invoice
   is submitted.
5. **Reading and correcting.** `GET /admin/einvoices` lists a tenant's (and optionally one store's)
   invoices with their state, behind `console.data.read`. `POST /admin/einvoices/{invoice_id}/correct`
   records an adjustment or a replacement (ADR-0225 decision 7) behind the new permission
   `fiscal.invoice.correct` (default OWNER and ADMIN), numbered under the tenant's correction symbol
   (ADR-0240 decision 6), audited, and queued for submission like any other invoice. Which errors need
   which kind is to be confirmed by legal or the accountant.

## Consequences accepted

- One additive migration, one alert kind, one permission, two admin routes. The permission snapshot
  grows.
- A store that never comes back is still the known gap ADR-0225 names.
- The cursor is per cell. A second cloud replica would read the same pages; the row's key makes that
  harmless, at the cost of a duplicate provider call that the provider's idempotency by invoice absorbs.
