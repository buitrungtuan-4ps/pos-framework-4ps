# ADR-0229 — The e-invoice issue moment is a schedule, held to the country's legal limit

**Status** Accepted 2026-11-24 by the owner · **Owner** @maintainers-architecture · **Date**
2026-11-24 · Phase 7 (roadmap-v4 row 7) · **Supersedes decision 3 of
[ADR-0225](0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)** (the issue
moment) and settles that record's open owner decisions · Relates to
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md),
[ADR-0223](0223-a-weighed-item-is-priced-by-its-weight.md)

**The owner decided on 2026-11-24:**

- **Vietnam first**, then the other countries.
- **Many vendors through adapters.** The e-invoice provider is chosen per tenant, brand or store through
  the provider catalogue (ADR-0153). Several vendors may be live at once, and adding one is one
  adapter that passes the `Fiscalization` contract suite. The sandbox is the reference adapter. Which
  real vendors to build first stays open.
- **The issue moment is flexible configuration**, a schedule with five modes, held to a fixed legal
  limit from the country pack.

Every statement of law below is **to be confirmed by legal or the accountant**.

## Context

ADR-0225 decision 3 offered three moments: off, every sale, or on request. The owner wants a store
to choose *when* as well. It can issue at once, after a delay per order, or in a daily batch.
Deferring the issue raises two questions. Which number does an invoice take when it is issued
later? And how late is too late in law?

## Decision

1. **`einvoice.issue_moment` is a schedule**, a published setting (ADR-0160) with five modes:
   - `EINVOICE_ISSUE_MOMENT_OFF`: no invoice. This is the register's default.
   - `EINVOICE_ISSUE_MOMENT_IMMEDIATE`: issued in the settle's own transaction. **The Vietnamese
     pack seeds it** (ADR-0183).
   - `EINVOICE_ISSUE_MOMENT_SCHEDULED_PER_ORDER`: each bill is issued `einvoice.delay_minutes` after
     its settle (0 to 1,440, default 0).
   - `EINVOICE_ISSUE_MOMENT_BATCH_WINDOW`: every bill waiting is issued together at the store's daily
     `einvoice.batch_minute` (minutes after local midnight, 0 to 1,439, default 1,320, which is
     22:00).
   - `EINVOICE_ISSUE_MOMENT_ON_REQUEST`: only a bill whose guest gave buyer details, at once.
2. **The legal limit is fixed law from the country pack.** A country states the latest moment an
   invoice may be issued after its sale. Vietnam's is **the end of the sale's calendar day**, the
   conservative reading, to be confirmed by legal. A country that states none has no limit. As with
   hand weighing (ADR-0223 decision 3), the rule is checked on both sides:
   - **The cloud refuses** a `einvoice.delay_minutes` that could cross the limit
     (`SETTING_BEYOND_COUNTRY_LIMIT`).
   - **The edge clamps** every due time to the limit. A batch set after a late sale, or a node from
     an older cloud, therefore issues at the limit rather than after it.
3. **A deferred invoice takes its number when it is issued, in issue order, not at settle.** This is
   the safer option:
   - **A reserved number can be orphaned.** A bill settled at 21:00 under a 22:00 batch may be
     voided at 21:30. If its number had been reserved at settle, the sequence would hold a number no
     invoice will ever carry, or the store would have to issue an invoice for a sale that no longer
     exists. Taking the number at issue leaves a voided bill with no number and no gap.
   - **Order matches dates.** Numbers taken at issue ascend with the invoices' own dates and times,
     which a gapless sequence per symbol is understood to need (ADR-0225 decision 2). Numbers
     reserved at settle and issued later in another order would not.
   - **Gapless still holds.** The store issues one invoice at a time under a lock, each number
     committed with its invoice in one transaction, as in IMMEDIATE mode.

   The cost is that a guest's receipt printed at settle shows no invoice number under a deferred
   mode. The receipt says the invoice will follow, and the number is on the invoice itself.
4. **Deferral is a durable record and a bounded queue.** A deferred settle writes
   `fiscal.invoice.scheduled` (version 1, additive: the bill, its due time, the buyer's
   `subject_id`) in its own transaction. The edge keeps the waiting bills in a queue of at most
   10,000, rebuilt from the log at start-up. A tick issues every bill that is due, in due order,
   each as `fiscal.invoice.issued`. A bill voided while it waits leaves the queue unissued. If the
   queue is full, a settle issues at once rather than refuse the sale or drop the invoice.
5. **The provider is resolved per store, at issue.** `fiscal.invoice.issued` carries, additively,
   the id of the e-invoice connection that served the store when the invoice was issued, taken from
   the published `integrations` node. The cloud submits each invoice through *that* connection. A
   store switched to another provider therefore sends only later invoices to it, and the earlier
   ones finish with the provider they were issued under.

## Consequences accepted

- Two settings (`einvoice.delay_minutes` and `einvoice.batch_minute`), a changed choice list for
  `einvoice.issue_moment`, a new event, an additive field and a new cloud refusal.
- A deferred mode shows no invoice number on the settle's receipt.
- The edge runs one more periodic task. A store that is switched off past its batch window issues
  late, at its next start. The limit then cannot be met, and the cloud's overdue alert names those
  invoices.
