# ADR-0035 — Retention is enforced by masking the subject store, not deleting it

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-20
**Relates to** [ADR-0027](0027-country-modules.md) · [ADR-0016](0016-postgres-access.md) · `docs/pos-spec.md` §15 · `docs/roadmap.md` Track A6, P7

**Context.** The system holds personal data even though it has no CRM: a marketplace order carries a
guest's name, phone, and delivery address; a corporate invoice carries the buyer's name, tax code,
and email. Vietnam's PDPD (Decree 13/2023), the GDPR, and the CCPA all require that such data not be
kept past the period it was collected for. Track A6 flagged the cron that enforces this as *unbuilt
anywhere*. Two facts shape where it acts: personal data is **never in the event log** — events carry
only a `SubjectId` ([`pos_proto::pii`], `docs/pos-spec.md` §15) — and the personal record lives in a
separate **subject store** keyed by that id, whose stated guarantee is that *financial figures still
reconcile after a person is anonymised*.

**Decision.**

- **Enforce retention by masking the subject-store row, not deleting it.** When a record is past its
  retention period the cron replaces every personal field's *value* with `[REDACTED]` and stamps
  `masked_at`, keeping the `subject_id` and `collected_at`. Masking rather than row-deletion is what
  preserves the reconciliation guarantee: an invoice still references a live subject id, so the books
  still balance, but nothing personal remains behind that id. `collected_at`/`masked_at` are an audit
  trail (what was held, when it was scrubbed), not personal data. Masking is **one-way** (a fixed
  sentinel, not reversible encoding) and **idempotent**.

- **The retention period is configuration, never a code default.** How long data may be kept is a
  legal decision. It comes from the config tree, defaulting per country ([ADR-0027](0027-country-modules.md)),
  and the cron does nothing until it has an explicit value — because masking on a guessed schedule
  errs either way (too soon erases data the business still needs; too late is the violation this
  exists to prevent).

- **A bounded, idempotent, daily sweep.** The sweep reads only unmasked records collected at or
  before the cutoff, in bounded pages, so a large backlog never loads the whole table and a record is
  never revisited once masked; a crashed sweep simply resumes, and a failed run is logged and retried
  next interval rather than crashing the cloud. Daily is ample for a period measured in months.

- **Scope, deliberately drawn.** This cron enforces the *automatic, time-based* policy over
  **customer and buyer** data only. It never touches employee data — there is no employee-behaviour
  monitoring in this system, and telemetry is machine data. And it is **not** the mechanism for an
  individual's erasure, access, or portability request: those are escalated to the Data Protection
  contact and actioned deliberately, per the organisation's policy, never on a schedule.

**What remains a human decision** (the cron enforces the period; it does not establish the basis):
confirming the lawful basis for processing under PDPD, the VPS-in-Vietnam data-residency requirement,
a lawful basis (DTA or explicit consent) for any cross-border transfer, and a DPIA for customer
analytics. These are recorded in the data-protection posture (A6), not in code.

**Rejected.**

- **Deleting the whole subject row** — rejected as the retention default: it orphans the `subject_id`
  every invoice and order references and breaks the reconciliation guarantee. (On-demand erasure of a
  single subject is a separate, escalated action, and can delete where a retention sweep masks.)
- **A hardcoded retention period** — rejected: a legally-loaded value must not be a code guess, so it
  is required configuration.
- **Scrubbing the event log** — rejected as unnecessary and impossible: the log has no personal data
  to scrub (PII-never-in-payload), and it is immutable by design.
- **Fulfilling rights requests on the cron** — rejected: the organisation requires access/erasure/
  portability requests be escalated, not actioned autonomously.

**Consequences.**

- No new dependencies. The engine — the record and its masking, the retention decision, and the
  sweep — is pure and I/O-free behind a `SubjectStore` seam, and unit-tested with no database or
  clock: masking scrubs every value while preserving the id and timestamps, is idempotent, leaks no
  original value, and the sweep masks exactly the records past retention and no others. Per the
  data-handling rules, the tests use only obvious placeholder values, never data resembling a real
  person.
- **Landed since:** persistence and wiring. `store-postgres` migration `0005` adds the `subjects`
  table (`subject_id` PK, `tenant_id`, `collected_at`/`masked_at` as epoch-ms, `fields` jsonb;
  RLS-isolated by tenant, with a partial index answering the sweep's "unmasked, past cutoff" query),
  and `PostgresSubjects` implements the `SubjectStore` seam — masking overwrites the field values in
  the row, so the PII is gone from the database, not merely flagged, and the `masked_at IS NULL`
  guard makes the write idempotent at the database too. `main` starts the daily runner **only when a
  retention period is configured** (`retention_days`); with none set the cron stays off (no code
  default — masking on a guessed schedule would erase early or keep too long). Proven by a
  `store-postgres` integration test (fetch-due → mask → not-re-fetched → not-re-masked) and a
  `pos-cloud` runner test (one sweep, then clean shutdown).
- **Deliberately not here yet:** the *writer* that populates the subject store — a marketplace
  order's or corporate invoice's buyer fields land there with P10/P11 — and reading the period from
  the country module's default rather than the process config. This ADR's engine, persistence, and
  cron are complete; what remains is the data source feeding it.
