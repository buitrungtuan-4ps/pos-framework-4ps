# ADR-0069 — Console audit trail: an append-only record of who changed what

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-27
**Relates to** [ADR-0034](0034-super-admin-auth.md) (admin auth) · [ADR-0067](0067-multi-admin-console-rbac.md) (per-admin identity + roles) · [ADR-0035](0035-retention-and-pii-masking.md) (retention/PII) · `docs/cloud-admin-ux-plan.md` (Track G2)

**Context.** The console has ~60 admin write routes and, until now, **none of them recorded anything**.
Before [ADR-0067](0067-multi-admin-console-rbac.md) there was a single anonymous super-admin enforced
at the schema level, so attribution was impossible by construction; G1 gave every operator a
distinct identity and role, which finally makes an accountable audit trail *possible*. This ADR adds
it: an append-only log of every console mutation — who did it, what they did, to which entity, and the
before/after of the change. The *domain* event log (ADR-0022) is already audit-grade for store
operations; this is its equivalent for **console administration**, which has no such record. Without
it there is no answer to "who revoked that API key?", "who archived that store?", or "what did this
price used to be?", and no basis for the accountability a multi-admin, compliance-bound deployment
(PDPD/GDPR) requires.

**Decision.**

- **One append-only `audit_log` table**, one row per console mutation, holding: a minted `id` (ULID);
  the acting admin as a **snapshot** (`actor_admin_id`, `actor_email`, `actor_role`) — copied at the
  moment of the action, so renaming or deleting an admin later never rewrites history; the `action`
  (`resource.verb`, e.g. `store.update`, `apikey.revoke`); the affected entity (`entity_type` +
  `entity_id`); the `before` and `after` of the change as `jsonb` (either nullable — `before` is null
  for a create, `after` for a delete); a nullable `request_id` for future request correlation; and the
  action instant `at` (Unix ms). Append-only is enforced at the grant: the query role gets `SELECT`
  and `INSERT`, never `UPDATE` or `DELETE` — a written audit row cannot be altered or removed through
  the application role.

- **`tenant_id` is nullable and the row is RLS-isolated by it.** Most actions are a tenant's data (a
  store rename, an API key) and are scoped to that tenant — including a *tenant create*, which scopes
  to the new tenant's own id, so the tenant's audit tab shows its own creation as the first entry. A
  few actions have no owning tenant (console admin management, the break-glass reset) and carry
  `tenant_id = NULL`. The table enables row-level security keyed on
  `app.tenant_id` exactly as `config_trees` does, so a query role sees only its own tenant's rows and
  never the global ones; the trusted pool-owner connection the server runs as bypasses RLS to write
  any tenant's row and to read across tenants for the console's audit screen.

- **The actor is threaded from the session, not invented.** Every `/admin` write route already
  resolves the acting [`AdminContext`](../../crates/pos-cloud/src/auth/admin.rs) through the
  permission guard ([ADR-0067](0067-multi-admin-console-rbac.md)); the audit entry's actor is that
  admin's id/email/role. There is no anonymous or system actor for a console mutation — a route that
  cannot name its actor does not write an audit row (and, being behind the guard, cannot run at all).

- **The append is best-effort, after the write, not inside its transaction — for now.** The `/admin`
  routes each use their own per-seam store adapter (registry, api-keys, webhooks, …) with no shared
  transaction, so making the audit write atomic with the mutation would mean threading a transaction
  through every seam — a much larger change than G2's first cut. So the audit append runs *after* the
  mutation succeeds and its failure is logged loudly (never silently dropped) but does not roll back
  the mutation. This mirrors the O1 liveness-capture posture (ADR-0068). The window it leaves — a
  mutation succeeds and its audit row fails to write — is small, logged, and closable later;
  transactional hardening (a shared `TxContext` across the admin seams) is a deliberate follow-up, not
  a v1 blocker. The table and seam are designed so that hardening changes only *where* `append` is
  called from, never the row's shape.

- **A dedicated `AuditStore` seam.** A trait (`append`, and a paginated `list` the audit screen reads)
  over an in-memory fake in tests and the `audit_log` table in the cloud — the same split every cloud
  store uses. The rich, filterable read (by actor, entity, action, time window) is a later slice; slice
  1 lands the table, the seam, the adapter, and a recent-first `list` sufficient to prove the
  round-trip.

**Rejected.**

- **Reusing the domain event log (ADR-0022)** for console actions — rejected: that log is the
  store's *business* event stream (orders, shifts), partitioned and replicated to the fleet; console
  administration is a different concern with a different audience (operators, not stores) and must not
  ride the store-replicated stream.
- **A mutable "last changed by" column per entity instead of a log** — rejected: it answers "who
  last touched this" but destroys the history (the price-change journal, the sequence of role
  changes) that an audit trail exists to keep. The append-only log keeps every change; surfacing
  `created/updated at·by` on a record (a later slice) is derived *from* it, not a replacement for it.
- **Transactional audit as a v1 requirement** — deferred, not rejected (see above): worth doing, but
  it gates on a cross-seam transaction the admin routes do not yet have, and blocking the whole track
  on it would leave the ~60 unaudited routes unaudited for longer.

**Consequences.**

- One additive migration (`0022_audit_log`), forward-only, RLS-isolated, `INSERT`/`SELECT`-only grant
  (append-only). No `PROTOCOL_VERSION` change; no permission-identifier change (reading the audit
  screen reuses `console.data.read` when it lands in slice 4).
- A new `AuditStore` seam with a store-postgres adapter and an in-memory fake, exercised by the
  real-PostgreSQL integration suite. Later slices thread the actor through the write routes and emit
  entries; this slice is the foundation they build on.
- Landed in slices under Track G2: (1) foundation → (2) registry-route auditing → (3) the remaining
  `/admin` write surfaces (api-keys, webhooks, config publish, admin management, rollup reset, device
  proposals, translations, catalog authoring + publish) → (4) audit read API + screen → (5) config
  version list/diff/rollback → (6) `created/updated at·by` surfacing + per-Detail audit tab. The
  break-glass reset writes its tombstone from the `reset_admin` binary, not an `/admin` route, so it
  is recorded when that path grows an audit-store handle rather than in slice 3.
- **PDPD/GDPR:** the log records *administrative* actions on business metadata (store names, config,
  keys), keyed to console operators — not customer or employee personal data, and not behavioural
  monitoring. `before`/`after` capture the entity's own fields (e.g. a store name, a config document),
  which are T2/T3 business data; a route whose entity carries personal data must redact it before
  writing the audit `before`/`after` (none in slices 1–3 do). Secrets are never written: an API-key
  entry records the granted scopes and expiry, and a webhook entry records the endpoint URL, but the
  key token and the HMAC signing secret — shown once to the caller — are excluded from `after`. An
  admin-management entry records the target admin's id (a ULID) and the role/status set, not an
  email. The log is subject to the same retention policy decision as other cloud data (ADR-0035).
