---
kind: added
upgrade_note: Nothing changes until a tenant writes `shift.drawer_access` `DRAWER_ACCESS_ASSIGNED` on Settings, confirming its notice; the default, `DRAWER_ACCESS_SHARED`, is every store today and writes the same bytes. `docs/snapshots/settings.txt` and `docs/configuration.md` gain the setting, honoured from 0.14.1, with its `confirm=` line. `PROTOCOL_VERSION` is unchanged. `cash.shift.opened` gains an optional field, left out where drawers are shared (`docs/snapshots/events.txt`). No route is added; `POST /api/shifts`, the settle and the paid in and out take optional fields, and a request without them works as before. No permission is new: the cash routes now check `cash.drawer.override_assignment` (migration `0086`), which leaves `NOT_ON_A_ROUTE`. No migration is added. Legal and the DPO sign off the notice's wording before the first live store that assigns its drawers (gate L7).
---
- **A drawer can belong to one person** (ADR-0167 decision 9, as ADR-0169 amends it). The
  `shift` node gains `drawer_access`, a setting at the four scopes: `DRAWER_ACCESS_SHARED`, the
  default, or `DRAWER_ACCESS_ASSIGNED`, under which a drawer is one person's for its session, under
  either drawer model.
  - **Written only with a recorded confirmation.** The Settings screen asks the person writing
    `DRAWER_ACCESS_ASSIGNED` to confirm that the tenant has a lawful basis for recording which
    employee is accountable for a drawer and has told its staff, in English and in Vietnamese,
    which names Decree 13/2023/NĐ-CP. The audit trail records who confirmed it, when and which
    notice. No layer write, rollback, node publish or release can turn it on.
  - The drawer is its starter's at its own till, and the store's one drawer is too. A drawer
    started for another till is the person `POST /api/shifts` names with `assigned_employee_id`,
    who must be on the store's roster; naming nobody on it is refused `409 ASSIGNEE_REQUIRED`.
  - `cash.shift.opened` records the person as `assigned_employee_id`, an id and never a name, and
    a restart rebuilds it. A session keeps its person until it closes, so a handover is a close
    and a new start.
  - Anybody else's cash at a settle, paid in, paid out, no-sale opening, count or close on that
    drawer, a batch close's included, is refused `409 DRAWER_ASSIGNED_TO_ANOTHER`, which names
    nobody, unless they hold `cash.drawer.override_assignment` directly or bring an approver who
    does for that act. The settle and the paid in and out take `approver_code` and
    `approver_pin` for it, and the approval is recorded as `security.permission.overridden`. One
    approver covers another till's drawer and its override together, and every drawer in a batch
    close. A card or any other tender is never refused.
  - `GET /api/shifts` and `GET /api/shifts/current` name the person by `assigned_employee_id`. The
    edge keeps no staff names, so the till shows none; it tells the signed-in person's drawer from
    somebody else's by id.
