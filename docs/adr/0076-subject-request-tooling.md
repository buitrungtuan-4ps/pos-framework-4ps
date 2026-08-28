# ADR-0076 — Subject-request tooling: per-subject PDPD/GDPR lookup, export, and erasure

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-28
**Relates to** [ADR-0035](0035-retention-and-pii-masking.md) · [ADR-0075](0075-media-and-file-rail.md) · [ADR-0067](0067-multi-admin-console-rbac.md) · [ADR-0069](0069-audit-trail.md) · [ADR-0016](0016-postgres-access.md) · `docs/cloud-admin-ux-plan.md` Track M5

**Context.** Track M5's final surface is the tooling a data subject's rights request is fulfilled with.
The pieces already exist and this ADR only adds the *deliberate, per-subject instrument* over them:

- Personal data lives in one place — the subject store ([ADR-0035](0035-retention-and-pii-masking.md)):
  a `subjects` row keyed by a `SubjectId`, holding the person's `fields` (`name`, `phone`, `address`,
  `email`, `tax_code`, …); events carry only the id, never the PII. The row is tenant-scoped
  (`tenant_id`, RLS) and the retention cron masks it once past its retention period.
- What is missing is the operator's path for an **individual** request: *access* / *portability* (show
  and export a subject's data) and *erasure* (mask it now, ahead of the retention clock). The retention
  sweep is explicitly *not* that path — it is time-based and runs on a cron; a subject request is
  deliberate and per-person.

Vietnam's PDPD (Decree 13/2023) and the GDPR both give a data subject these rights, and both make the
*process* — confirming a lawful basis, verifying identity, escalating an EU-resident request to the
Data Protection contact, honouring cross-border-transfer rules — a human obligation. This tool does not
replace that process; it is the instrument the Data Protection contact uses to carry it out, and it
records who did what so the fulfilment is auditable.

**Decision.**

1. **A `console.subjects.manage` permission — deny-by-default, owner-only.** This is the console's most
   sensitive T1 surface: it can read and irreversibly erase a person's data. It is therefore *narrower*
   than the Owner/Admin norm the other manage permissions use — only the owner holds it
   ([ADR-0067](0067-multi-admin-console-rbac.md)), and the server re-checks it on every route.

2. **Per-subject only, tenant-scoped — never bulk, never cross-tenant.** Every route takes one
   `SubjectId` and a `?tenant_id=`, and the store read returns the subject only if it belongs to that
   tenant. There is deliberately **no list-all and no bulk export**: an unbounded T1 export is the
   organisation's escalation case, not a product feature.

3. **Three routes, each audited with metadata only** ([ADR-0069](0069-audit-trail.md)).
   `GET /admin/subjects/{id}` looks a subject up — its existence, whether it is already masked, and the
   field *count* — **without** returning the values, so the operator confirms the right person before
   acting. `GET /admin/subjects/{id}/export` returns the record *with* its field values — the
   portability/access payload, the one route that returns PII, and only to the owner. `POST
   /admin/subjects/{id}/erase` masks the record. Every call writes an audit entry recording *who* acted
   on *which subject id* and *what action* — the export entry records the field count, **never the field
   values**. The dashboard makes erase a typed-confirm (the operator types the subject id), since it is
   irreversible.

4. **Erase reuses the retention masking, not a hard delete** ([ADR-0035](0035-retention-and-pii-masking.md)).
   Erasing masks every field value to the `[REDACTED]` sentinel in place and stamps `masked_at`, keeping
   the `subject_id` and `collected_at` — so an invoice that references the subject still reconciles and
   the books stay balanced, while the personal data is genuinely gone from the row. It is irreversible
   and idempotent: erasing an already-masked subject is a no-op that still returns success.

5. **This tool is the Data Protection contact's instrument, not an autonomous fulfiller.** It records
   and enables a human decision; it does not make one. The console surfaces a standing reminder on the
   screen: confirm the lawful basis and the subject's identity before processing; confirm consent,
   retention, and (for customer analytics) a DPIA; and **escalate an EU-resident rights request to the
   Data Protection contact** rather than actioning it here. Cross-border transfer of the exported data
   is the operator's obligation to clear (a DTA or explicit consent) — the tool hands the payload to the
   authorised operator and logs that it did.

**Rejected.**

- **A hard `DELETE` of the subject row** — rejected: it breaks reconciliation (an invoice would dangle)
  and loses the `masked_at`/`collected_at` audit trail. Masking in place removes the PII while keeping
  the non-personal skeleton, which is [ADR-0035](0035-retention-and-pii-masking.md)'s settled posture.
- **A bulk / list-all / cross-tenant export** — rejected: that is the escalation case the organisation's
  data-classification policy names, not a feature. The tooling is per-subject by construction.
- **Owner *and* admin** — rejected for this surface: the erase is irreversible and the export is raw T1,
  so the grant is owner-only, tighter than the other manage permissions.
- **Auto-fulfilment on a request record** — rejected: fulfilment is a human obligation (identity, lawful
  basis, EU escalation); the tool assists it and audits it, it does not automate it.

**Consequences.**

- No schema change: the `subjects` table ([ADR-0035](0035-retention-and-pii-masking.md), migration 0005)
  already carries `tenant_id`, `fields`, and `masked_at`. The `SubjectStore` seam gains a per-subject
  `fetch`; erase reuses the existing `save_masked`.
- One new permission (`console.subjects.manage`, owner-only); additive routes; no wire or edge change,
  and no `PROTOCOL_VERSION` bump (nothing here crosses the edge).
- Every lookup, export, and erasure is audited (actor + subject id + action; export records the field
  count, never the values), so the fulfilment of a rights request is itself auditable.

**Deferred / flagged follow-ups.**

- A durable **request register** (log each incoming subject request, its lawful basis, and its
  resolution) — a workflow layer above this tooling, deferred to a compliance-led change.
- The **T1/T2 CSV export & import** domains ([ADR-0075](0075-media-and-file-rail.md) decision 5 — the
  employee roster, per-channel prices, and item import) remain deferred to a human-reviewed DPIA; they
  are not unlocked by this ADR.
