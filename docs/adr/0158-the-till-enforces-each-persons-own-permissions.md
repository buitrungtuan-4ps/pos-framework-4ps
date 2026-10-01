# ADR-0158 — The till enforces each person's own permissions, and a role is whatever the business composes

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-26
· Relates to [ADR-0070](0070-people-and-access.md), [ADR-0084](0084-device-authentication.md),
[ADR-0091](0091-durable-edge-auth-state.md), [ADR-0118](0118-one-credential-per-box-and-the-cloud-learns.md),
[ADR-0122](0122-a-store-group-is-a-delivery-cohort.md), [ADR-0115](0115-reason-codes-are-a-managed-list.md)

## The problem

The owner decided on 2026-09-26 that the business creates roles freely and gives each person the
roles they need. Half of that exists. [ADR-0070](0070-people-and-access.md) made a role tenant data —
a name, a list of catalogue permissions and a discount ceiling — authored in the console with no code
change, and assigned to a person at a store.

The other half does not: **the store does not enforce the role.**

- `decision_ctx` in `crates/pos-edge/src/app.rs` decides every command with a store-wide set that is
  `Permission::ALL` and is never narrowed — the deferral ADR-0084 S0b recorded. A person's own role
  counts in four places only: device management, the discount ceiling, who may approve, and who may
  sign in.
- Fourteen of the 24 permissions are enforced nowhere, and many state-changing routes name no
  permission at all: seat and clean a table, add and fire a line, open a bill, settle, split and
  merge, bump a ticket.
- A person holds one role per store; an archived role still compiles into the published node; a
  change reaches a store only when someone presses publish.
- The approval PIN has no lockout, and the person acting may approve their own action.
- `/api/pair/devices` and `/api/pair/revoke` need only a paired device, not `admin.device.manage`.
- The till cannot see what the signed-in person may do, so it shows every control to everyone.
- In the console, `GET /admin/employees` needs only `console.data.read`, so Viewer and Ops read
  staff names — personal data under Decree 13/2023, contrary to ADR-0070's delivery note.

## Options considered

| | Option | Why not / cost |
|---|---|---|
| A | Keep the store-wide set | The decision is not met: a role changes nothing at the till |
| B | A fixed ladder of built-in roles | Contradicts "create roles freely"; every new job title becomes a release |
| C | **Each person's own set, deny by default, every route names its permission, approval set per role** | Staff lose actions their role never granted — which is the point — so it needs a rollout switch |

## Decision

Option **C**. The owner approved it on 2026-10-01.

1. **Per person, deny by default.** The edge decides every command with the signed-in person's own
   permission set from the published `permissions` node. The store-wide set is removed. A person the
   node does not list holds nothing.
2. **Every state-changing route names one permission.** The catalogue grows, additively, to cover
   what is open today — indicatively `sales.table.manage` (seat, clean, release), `sales.line.add`,
   `sales.line.fire`, `sales.ticket.bump`, `billing.bill.open`, `billing.bill.split` (split and
   merge) and `billing.payment.take`; the permission snapshot is the record. A test pins the
   route-to-permission table, so a new route without a permission fails CI.
3. **A person may hold several roles, and an assignment may reach beyond one store.** Their
   permissions are the union and their discount ceiling the highest. An assignment names a store, a
   store group ([ADR-0122](0122-a-store-group-is-a-delivery-cohort.md)) or every store of the tenant.
4. **Approval is set per role, not fixed in code.** A role grants each permission either *directly*
   or *with approval*: another person, holding it directly, authorises that one action with their
   code and PIN, and `security.permission.overridden` records who. The catalogue's `pin` flag is only
   the default a new role starts from, so every role begins where today's behaviour is.
5. **Approval is hardened.** A wrong approval PIN counts toward the approver's sign-in lockout
   (ADR-0030; five failures and five minutes today, both settings under ADR-0160). The owner asked on
   2026-09-30 for that part now, so it lands ahead of this record. The approver must also be a
   different person from the actor. That part lands with per-person enforcement, because until then
   a manager alone on shift has nobody else to approve their own void.
6. **The till knows the person.** `GET /api/session` returns the person's permission ids and, for
   each, direct or with approval. The till hides what the person cannot do and asks for an approver
   where one is needed; the edge stays the authority.
7. **What the console authors is what the store enforces.** An archived role contributes nothing.
   Removing an assignment, archiving a person or archiving a role publishes the `permissions` node to
   every store it affects at once; other changes publish as today. The assignment route refuses an
   unknown or archived person, store or role, as ADR-0070 already required.
8. **Device routes** `/api/pair/devices` and `/api/pair/revoke` require `admin.device.manage`, as
   minting a pairing code already does (ADR-0118).
9. **Staff records in the console** need a new `console.people.read` (Owner and Admin by default)
   rather than `console.data.read`.
10. **The node has one type.** `pos_proto::people::PublishedPermissions` replaces the two private
    structs the cloud and the edge each keep today, so the two sides cannot drift.

**Rollout.** A migration grants each permission added by item 2 to every existing role, so no role
loses an action merely because the action gained a name. Enforcement is a per-store switch in the
`permissions` node, `enforced`: on for stores created after this lands, off for the stores that exist.
Before an owner turns it on, the console lists which assigned roles at that store lack which
permissions. The switch is a rollout aid; once every store runs with it on, a later change removes
it. New tenants start with editable roles — Owner, Manager, Supervisor, Cashier, Server and Cook,
today's `default_grants` — which they may rename, change or archive.

## Before this is accepted

- The owner confirms the rollout switch, and that it is temporary.
- The owner confirms that one assignment may cover every store of the tenant (an area manager).
- Out of scope, named so it is not assumed: console admin roles stay the four fixed roles of
  ADR-0067; per-device limits (a kitchen screen that can only bump) come with the till's role-aware
  menus; percentage discount ceilings and price-override limits come later.

Met on 2026-10-01: the owner confirmed the rollout switch and that it is temporary, and that one
assignment may cover every store of the tenant. The items named out of scope stay out of it.

## Consequences accepted

- **A cashier can be refused an action they took yesterday** once their store turns enforcement on.
  That is the decision working; the switch and the console's check are how it arrives without a
  surprise in the middle of service.
- **Roles now matter, so the console must make them easy to author**: permissions grouped, starting
  roles, and the per-store check above.
- **The catalogue grows toward its bound.** `PermissionSet` is a `u64`; after item 2, ADR-0159's
  `billing.fee.waive` and ADR-0160's `reports.takings.view`, 33 of 64 bits are used. Widening it is a
  later, mechanical change.
- **`pos-core` and `pos-proto` change** — the catalogue, the decision context and the node type — and
  need the owner's review under AGENTS.md §6.
- **No new personal data, and no staff monitoring.** Events already carry the acting `employee_id`;
  this record changes what a person may do, not what is recorded about them. A durable sign-in log is
  not introduced here; it would need its own lawful basis, retention and DPIA (ADR-0035, ADR-0070).
