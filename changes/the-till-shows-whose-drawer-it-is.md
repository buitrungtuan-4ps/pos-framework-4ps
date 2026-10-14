---
kind: added
---
- **The till works with assigned drawers** (ADR-0167 decision 9). Where the store assigns drawers,
  the Shift screen says of this till's drawer, and of each drawer in the list, whether it is the
  signed-in person's or another person's, never by name. On another person's drawer, a manager's
  code and PIN are asked for in place from anyone who does not hold
  `cash.drawer.override_assignment` directly, and the paid in and out, the count, the close, a
  no-sale opening and a cash payment on the Pay screen carry them. Starting another till's drawer
  asks for the staff code of the person it is for. The two refusals, `DRAWER_ASSIGNED_TO_ANOTHER`
  and `ASSIGNEE_REQUIRED`, have their own sentences in English and Vietnamese.
- `GET /api/shifts` says `drawer_access: DRAWER_ACCESS_ASSIGNED` where drawers are assigned, and
  nothing where they are shared. `POST /api/shifts` takes `assigned_staff_code`, the staff code of
  the person another till's drawer is for, read where `assigned_employee_id` is absent. Both are
  additive; a store whose drawers are shared answers as before.
