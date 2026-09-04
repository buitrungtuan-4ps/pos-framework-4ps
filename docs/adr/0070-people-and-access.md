# ADR-0070 — People & access: employees, store assignments, role templates, and the permissions a store enforces

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-08-27
**Relates to** [ADR-0067](0067-multi-admin-console-rbac.md) (console admin identity + roles) · [ADR-0030](0030-pairing-and-offline-auth.md) (edge offline PIN auth) · [ADR-0033](0033-config-tree.md) (config tree the edge pulls) · [ADR-0035](0035-retention-and-pii-masking.md) (retention / PII) · [ADR-0069](0069-audit-trail.md) (audit trail) · `docs/cloud-admin-ux-plan.md` (Track M1)

**Context.** The console has no notion of a store's **people**. The runtime already authenticates staff
at the edge with a PIN (ADR-0030) and `pos-core` already owns the *permission catalogue* (the set of
capabilities a role may hold, §9), but there is nowhere in the cloud to author *who* works at a store,
*what* they may do, and to push that to the store so the edge enforces it. Today a store's staff list
and their permissions live only on the edge, set out-of-band; there is no central roster, no
role templates, no way for an operator to onboard or offboard a cashier from the console, and no audit
of who changed a person's access. Track M1 adds this: **employees** (identity + a set PIN), **role
templates** over the `pos-core` permission catalogue, **per-store assignments** binding a person to a
store with a role, all compiled to a `permissions` node on the store's config tree that the **edge
applies** so an `EdgeSession` authorises staff from the published set rather than local guesswork.

This is the console's first **T1 Restricted** data: an employee's name, staff code, and PIN are
personal data of an identifiable individual, and — for a Vietnamese deployment — squarely inside
**PDPD (Decree 13/2023)**. The design below is written so the *system* is safe by construction; the
*operational* obligations (lawful basis, retention period, DPIA, consent/notification) are a
deployment decision the operator confirms, surfaced here and in the console, not something code can
assert on their behalf.

**Decision.**

- **An `employees` table, tenant-scoped and RLS-isolated** — unlike the *console-admin* tables
  (ADR-0067), which authorise the privileged console and carry no tenant, employees are a **tenant's**
  data, so the table carries `tenant_id`, row-level security on `app.tenant_id` exactly as `stores`
  and `config_trees` do, and the per-tenant grant. One row per person: a minted `id` (ULID), the
  owning `tenant_id`, a human `name`, a tenant-unique `code` (the staff/badge code an operator types),
  a `status` (`active`/`archived`), and `pin_phc` — the **Argon2id** hash of the set PIN, nullable
  until a PIN is set, **never the PIN itself**. `created_at`/`updated_at` are kept for the record.
- **Per-store assignment is a join table** (`employee_store_assignments`): an employee works at zero
  or more of their tenant's stores, each assignment carrying the `role_template_id` that store grants
  them. All three ids belong to the same tenant — the `tenant_id` column + RLS isolate the rows, and
  the route layer checks referential validity before a write (the schema follows the codebase's
  soft-reference convention — e.g. `devices.store_id` — rather than cross-table foreign keys under
  RLS). Unlike employees and roles, an assignment is a plain grant that is **removed** — removing it
  offboards the person from that store without deleting the person.
- **Role templates map a name to a set of `pos-core` permissions** — a `role_templates` table holds a
  tenant's named roles (e.g. *Cashier*, *Shift lead*, *Manager*), each a stored set of permission
  identifiers drawn from the **`pos-core` permission registry** (§9), the single source of truth for
  what capabilities exist. The console never invents a permission string; it offers the catalogue and
  stores a subset. Templates are per-tenant so one tenant's *Manager* is not another's.
- **The PIN is set/reset, never read.** Setting a PIN hashes it with Argon2id (the same primitive as
  the admin password, ADR-0067) and stores only the PHC; a reset overwrites the hash. The cloud never
  returns a PIN, and the edge verifies against the published hash offline (ADR-0030) — the PIN travels
  to the store as a hash, never in the clear. A four-digit PIN's defence is the Argon2id cost plus the
  edge's attempt rate-limit, not secrecy of a long secret; the console warns against reusing PINs.
- **Compile to a `permissions` config node the edge applies.** Publishing resolves a store's active
  assignments into a flat, edge-shaped document — per employee: `id`, `code`, `name`, the granted
  permission set (roles flattened to permissions), and the PIN hash — and writes it onto the store's
  `permissions` config node, so it rides the **config tree** to the store like every other config
  change (ADR-0033): no new channel, no new sync path. The edge's `EdgeSession` gains the permission
  set from this node and authorises staff from it (a later M1 slice), replacing any local roster.
- **Audit records access changes without leaking PII.** Every employee/role/assignment write emits an
  audit entry (ADR-0069) keyed to the acting operator, but the `before`/`after` record the employee
  **id, code, status, and role** — **not** the name, and **never** the PIN or its hash. So the trail
  answers "who gave this badge manager access, and when?" without turning the audit log into a second
  copy of the staff directory.

**PDPD / T1 posture (surfaced, not asserted).**

- **Lawful basis & notification.** Storing employee PII needs a lawful basis (typically the employment
  relationship / legitimate interest) and staff notification under PDPD. The console surfaces this as a
  confirmation the operator makes; the code does not presume consent.
- **Data minimization.** The model stores only what access control needs — identity, code, role, PIN
  hash. No contact details, no biometrics, no behavioural or location data. This is **access
  management, not employee monitoring**: there is deliberately no tracking of where staff are, what
  they view, or their communications.
- **Retention.** Employee rows follow the same retention decision as other tenant data (ADR-0035);
  offboarding archives (does not immediately delete) so the books stay reconcilable, and a
  retention/erasure request is handled through the Data Protection contact, not an ad-hoc delete.
- **Cross-border.** Nothing here transfers data across a border; a deployment that replicates the
  cloud across regions must confirm a DTA/consent first (flagged, not code's call).
- **DPIA.** A first-time deployment of a staff directory should complete a DPIA; the ADR notes it so
  the operator does not skip it.

**Slices (Track M1, one PR).**

1. **Foundation (this):** the `employees` schema (migration `0023`), the `EmployeeStore` seam
   (create / list / get / update / set-or-reset PIN / archive), the `store-postgres` adapter, an
   in-memory fake, and unit + real-PostgreSQL tests (including the RLS/grant and the PIN-hash-never-raw
   invariants). No routes yet — the store the later slices build on.
2. **Role templates + per-store assignments:** the `role_templates` and `employee_store_assignments`
   tables + seam methods, over the `pos-core` permission catalogue.
3. **Admin routes + audit:** `/admin` CRUD for employees, roles, and assignments, with PIN set/reset,
   plus the permission catalogue the role editor offers; every write audited (id/code/role, never
   name/PIN). Reads sit behind `console.data.read`; every write behind the new
   `console.people.manage` (Owner/Admin) — the permission is introduced here, where the routes first
   need it, and surfaced in the UI by slice 4.
4. **Console screen:** a People screen on the F2 kit — roster, role editor, per-store assignment, PIN
   reset — gated by `console.people.manage`.
5. **Publish `permissions` config node:** a pure compiler turns a store's active assignments into the
   edge-shaped document (per staff: id, code, name, the flattened permission set, and the PIN hash),
   and `POST /admin/people/publish` writes it onto the store's `permissions` config node, versioned
   through the config tree like catalog/layout. Behind `console.config.publish`; the audit records the
   config version and staff count, never a name or PIN.
6. **Edge applies (this):** the edge's config-pull rebuild reads the `permissions` node into a
   `StaffRoster` on the `EdgeSession` (code → granted permission set + PIN hash), and
   `EdgeSession::authorise_staff(code, pin)` verifies a sign-in against the published Argon2id hash
   (ADR-0030), yielding the person's permission set. A permission id the running edge predates is
   dropped, not fatal; an absent or malformed node leaves the roster unchanged, the same
   safe-by-default rebuild as the menu. The store now authorises from the published set, not a local
   roster.

**Consequences.** The console becomes the source of truth for store staff and their access, auditable
and PDPD-aware. Additive throughout: new tenant-scoped tables and a new config node; no
`PROTOCOL_VERSION` change (the `permissions` node rides the existing config tree). The permission
*catalogue* stays in `pos-core`; this ADR only adds the *assignment* of those permissions to people.
The break-glass and console-admin auth (ADR-0067) are unaffected — console admins authorise the
console; employees authorise the store.

## Delivery note — the assignment read names its person (2026-09-04)

`GET /admin/assignments` returned three ids and nothing else, so the People screen labelled an
assignment by searching the tenant's whole roster for the matching employee. That works until the
roster is paged, and paging it is what
[ADR-0098](0098-paged-admin-reads.md)'s B3-4 note deferred for exactly this reason. The resolution
moves onto the read that needs it: a `LEFT JOIN` on `employees` puts the person's `name` and `code`
on the assignment row.

Three things worth recording, because each was decided by looking rather than by assuming.

**Why `LEFT`.** `employee_store_assignments` declares no foreign key to `employees` — migration 0024
has none, and nothing since adds one. So the schema permits an assignment naming a row that is not
there. An inner join would drop it from the list, which hides a grant that still exists: the
console would show nothing while the store still authorises the person. A left join lists it with a
null name, which the console renders as the id — precisely what it did before this read resolved
anything. Both directions are pinned by an integration test, and swapping `LEFT` for `INNER` fails
it.

**This is not an expansion of T1 exposure.** The name and code are personal data, and the read that
carries them used to carry none. But the caller reaches this endpoint through
`console.people.manage`, the same permission that lets them read the roster in full, and the screen
already displayed the name — by fetching the entire roster to find it. The same person sees the same
data; it arrives on a smaller response instead of a larger one. `pin_phc` is not selected here, as
it is not selected on any read. Data minimization improves rather than regresses: labelling one
store's assignments no longer requires downloading every employee in the tenant.

**The screen still does not page.** Removing the roster read from the labelling was one of three
blockers. The assign picker still offers every active employee out of the loaded set, and giving the
table a page would leave the picker offering only whoever landed on it. That needs a searching
picker over a server-side employee search — and `GET /admin/employees` has no `q`: the `q`/`sort`/
`order` that B3-3 gave the other four paged reads never reached employees, because employees were
held back to B3-4. So the remaining order is: search the roster server-side, put the picker on it,
then page the table.

## Delivery note — the assign picker searches instead of holding (2026-09-04)

The second of the three steps the note above laid out. `GET /admin/employees` takes a `?q=`, and the
People screen's assign picker calls it with a short limit instead of filtering a roster it keeps in
memory.

**The gap this closed was in an earlier slice's coverage, not in a design.** B3-3 gave `q`, `sort`
and `order` to the four paged reads it touched; employees were not among them, because employees had
been held back to B3-4 pending the owner's T1 call. So the cohort that "all" got a search was in fact
four of five, and nobody noticed until the picker needed the fifth. Recorded because the shape
generalises: a slice that says "the cohort" is worth re-checking against the cohort's actual members
when a later slice depends on it.

**`sort` and `order` are still absent, and that is a decision.** The other four got them because
their screens have sortable table headers pointing at server fields. The People table sorts
client-side over rows it already holds. Adding server ordering now would be surface with no
consumer; it belongs to the slice that pages the table, where the headers are what make it
necessary.

**The search matches name or code, and neither alone would do.** Those are the two handles an
operator has on someone: a name they were told, or the code on a badge. It does not match `pin_phc` —
not selected by any read, and a substring predicate over an Argon2id hash could only ever leak timing
about a secret.

**An archived match is shown disabled, not filtered out.** Filtering would have been fewer lines, and
wrong: an operator searching for a real person would see an empty list and be unable to tell "no such
person" from "that person is archived". The disabled row answers the question they actually have.

What is left for step three is the employees table itself — now the screen's only reader of the whole
roster.

## Delivery note — the roster page can be ordered (2026-09-04)

`GET /admin/employees` takes `?sort=newest|name|code` and `?order=asc|desc`. Nothing in the console
calls them yet, and that is deliberate: this is the half of the third step that has to exist before
the other half can be written.

**Why it is not speculative now, having been exactly that a slice ago.** The previous note argued
against adding these because no screen needed them. Building the picker made the need concrete:
`employeeColumns()` gives `name` and `code` a `sortValue`, so the People table's headers sort
client-side today, and `DataTable` decides a table is server-sorted by whether the caller offered
`onSort`. Server-paging the table without these parameters would therefore not "defer" header
sorting — it would silently delete it, or worse, leave headers that reorder twelve visible rows and
look like they reordered the roster. The requirement was discovered by reading the consumer, not
predicted.

**Every order is total, and the tiebreaker flips with the direction.** `ORDER BY name` is no more
total than `ORDER BY created_at` was: two employees sharing a name is ordinary — it is one of the
reasons a staff code exists — so each order ends in the primary key. The descending variants reverse
the tiebreaker too, so `?order=desc` is the exact reverse of the ascending page rather than a
different total order that happens to share a first row. That distinction is invisible until two
rows tie, which is why the test seeds a shared name: with distinct names the mutation that stops
flipping the tiebreaker passes.

**The `code` order gets no new index and the `name` order does.** `employees_code_key (tenant_id,
code)` from migration 0023 already covers the code order, and because it is UNIQUE the `id`
tiebreaker there can never fire — `code` alone already orders a tenant's rows totally. The read
appends it anyway, so that the totality rule holds by construction rather than by a reader
remembering which column happens to have a unique index behind it. Names are not unique, so
migration 0046 adds `employees_by_tenant_name (tenant_id, name, id)`; an `EXPLAIN` test asserts the
plan walks it with no `Sort` node above the scan, and deleting the migration fails that test.
