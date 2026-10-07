# ADR-0167 — A till has its own cash drawer

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-06,
revised and accepted 2026-10-07
· Extends [ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0165](0165-cash-paid-in-and-out-is-counted-in-the-drawer-and-a-no-sale-opening-needs-a-manager.md)
· Relates to [ADR-0112](0112-print-agents.md), [ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md)

## The problem

On 2026-10-01 the owner chose one drawer per till for new stores, a model ADR-0160 offers only once
multi-drawer shifts land (decisions 2 and 5). Nothing honours it: the edge holds one open shift per
store (`Projection::open_shift`), and the kick goes to the first printer marked `drawer_attached`
(`printing::drawer_printer`), so cash taken at the bar springs the counter's drawer. A till is a
`TERMINAL` entry on the `devices` node, which a paired device is once bound (ADR-0112). On
2026-10-07 the owner asked for the world's standard practice, with each policy choice configurable.

## Options considered

| | Option | Cost |
|---|---|---|
| A | One store shift, with a count per drawer at its close | Every cash event needs a drawer, and no drawer starts late or closes early |
| B | **A session per drawer, one drawer per till**, as Toast, Square, Simphony and Lightspeed keep them | The shift machine, blind count and report are reused per drawer; cash events name the till |
| C | A drawer per cashier (`PER_CASHIER`) | Cash figures per person by design, which nobody has asked for |

## Decision

Option **B**. Each choice a business makes differently is a setting in ADR-0160's register, at
tenant, brand, store group or store, with its own `since`, so the console honours or hides it
(decision 5). Who may act is a role's permission (ADR-0158).

1. **`shift.drawer_model`**: `DRAWER_MODEL_PER_STORE`, the default and today's one drawer, or
   `DRAWER_MODEL_PER_TERMINAL`, the preset for a new store. `PER_CASHIER` is not added, since item 9
   gives each person's accountability without it. This narrows ADR-0160 decision 2.
2. **A session per drawer.** Under `PER_TERMINAL` each till's drawer runs ADR-0165's session on its
   own (float, cash taken, paid in and out, no-sale openings, expected amount, count, over/short),
   and starts late or closes early without touching the others. A session is neither a person's
   time clock nor the business day, which stays the automatic `business_date` cutoff.
3. **Managed from any till.** A holder of `cash.shift.open` starts its till's drawer, and a holder
   of `cash.shift.close` counts and closes it, at that till. To start, count or close another till's
   drawer from any till, one, several or all at once on a screen listing every till with its
   default float, a person also holds a new PIN-flagged permission, `cash.shift.manage_other_till`
   (default roles supervisor, manager and owner), directly or approved for one act (ADR-0158
   decisions 4 and 5), so a cashier keeps to their own till's drawer unless a manager approves.
   Counting goes drawer by drawer, blind per `shift.blind_close`. No device is the main POS. A paid
   in, a paid out and a no-sale opening are made at the till whose drawer they spring.
4. **A default float per till**: an optional `opening_float_minor` on the `TERMINAL` entry, set on
   the console's Devices screen within `shift.opening_float_minor`'s bounds, which applies without
   it (ADR-0160 decision 4).
5. **Events name the till** with an optional `terminal_device_id`, the `TERMINAL` entry's
   `device_id`, never read as the envelope's `device_id` (`docs/naming-and-api.md` §2), on
   `cash.shift.opened`, `cash.shift.closed`, `cash.drawer.paid_in`, `cash.drawer.paid_out` and
   `cash.drawer.opened`. Absent is the store's drawer, so a `PER_STORE` store writes today's bytes.
   A payment and `cash.shift.counted` name the drawer's shift.
6. **Cash goes into the drawer of the till it is taken at**, by the device's binding to a terminal.
   Under `PER_TERMINAL` a device that is no till takes other methods but no cash
   (`409 NOT_A_TILL`), and an unreadable binding refuses the cash (`503`) rather than guess, as #631
   refuses to print. The kick, under ADR-0165's rules, opens that till's drawer at the receipt
   printer its terminal names, directly or in a print agent's job as ADR-0112 describes, which lifts
   the last condition of ADR-0165 decision 4. A till with no drawer answers `NO_DRAWER`.
7. **Every drawer springs by itself before `PER_TERMINAL` is offered**: the agent's kick ships
   first, and `PER_TERMINAL`'s `since` is the release that ships the rest. An older store reached
   through a wider scope ignores the field and keeps one drawer, and the console marks it.
8. **A change of model waits for a boundary**, when no session is open; the Shift screen says so.
9. **`shift.drawer_access`**: `DRAWER_ACCESS_SHARED`, the default, where anyone permitted takes cash
   at a till into its drawer, or `DRAWER_ACCESS_ASSIGNED`, Toast's cash drawer lockdown.
   - An assigned drawer belongs, for its session, to whoever starts it at its till, or to the
     person a manager names when starting it elsewhere. A handover is a close and a new start.
   - Only that person takes cash into it, pays in or out, opens it without a sale, counts or closes
     it. Anyone else is refused (`409 DRAWER_ASSIGNED_TO_ANOTHER`) unless they hold a new
     PIN-flagged permission, `cash.drawer.override_assignment`, directly or approved for one act
     (ADR-0158 decisions 4 and 5).
   - It applies under `PER_STORE` too, since accountability belongs to a drawer however many there
     are: the store's one drawer is then one person's.
   - `cash.shift.opened` records the person as `assigned_employee_id`, an `EmployeeId` the
     personal-data fence already admits, never a name. That drawer's over/short is one person's
     figure, so where the setting is written the console says the tenant needs a legal basis and
     must have told its staff (Decree 13/2023). Nothing ranks or compares people, and the console
     names the person only under `console.people.read` (ADR-0158 decision 9).
10. **`shift.close_report`**: `CLOSE_REPORT_PER_DRAWER`, the default, where each drawer's report
    prints where it is closed, as the one report does today; `CLOSE_REPORT_COMBINED`, where closing
    several prints one slip, the store's totals then each drawer; or `CLOSE_REPORT_NONE`, where
    nothing prints and the console has it.
11. **`shift.drawer_day_end`**: `DRAWER_DAY_END_FLAG`, the default, where a session open past its
    business day's cutoff is flagged on the till and in the console and keeps working, or
    `DRAWER_DAY_END_REQUIRE_CLOSE`, where it takes no more cash until counted and closed
    (`409 DRAWER_DAY_ENDED`) and other methods still work.
12. **`shift.variance_reason_minor`**: a whole amount in minor units, `0` to `1000000000`, where
    `0`, the default, is off. A close whose over/short exceeds it in absolute value records a
    reason, `cash.shift.closed`'s new `reason_code_id`, for a new action,
    `REASON_ACTION_CASH_VARIANCE` (ADR-0115), which the framework's default list covers. A counted
    session cannot be counted again, and only the close after the count asks for the reason
    (`409 VARIANCE_REASON_REQUIRED`), so a blind close stays blind. A store whose list offers no
    such reason still closes, and the console warns.
13. **Roles decide the rest.** Who starts, counts and closes a till's own drawer is
    `cash.shift.open` and `cash.shift.close`, and who does it for another till is
    `cash.shift.manage_other_till`, per role. Under `PER_STORE` the one drawer is every till's, so
    nothing changes there. The console shows each drawer's figures
    under `console.reports.revenue`, as the store's: a till's cash is the store's money split by
    till, so no console permission is added. The Today tile stays the store's figure
    (`docs/pos-spec.md` §17).

## Consequences accepted

- **Additive.** `PROTOCOL_VERSION` stays 1 ([ADR-0024](0024-protocol-version-negotiation.md)).
  `events.txt` gains seven fields, `settings.txt` five settings, `permissions.txt` two permissions,
  and `routes.txt` one read, `GET /api/shifts`: each till's drawer state and default float, never an
  expected amount where the close is blind.
- **Three additive migrations**: the terminal's float on `device_proposals`, as 0082 added its
  receipt fields, and two grants, of `cash.shift.manage_other_till` (slice 3) and of
  `cash.drawer.override_assignment` (slice 7), each once, directly to each role granting
  `cash.drawer.open_no_sale` directly and with approval to the rest, as 0076 did. Each grant queues
  its stores for the people republisher, as 0083's header says. The rollup per drawer is jsonb.
- **Offline.** All of it is the edge's: with the line down every till trades, starts, counts and
  closes, and an override is approved against the roster it holds.
- **Under `NO_SHIFT_SELLING_REFUSE`**, cash needs the till's own drawer open, and other methods any
  open drawer. A drawer belongs to its terminal, so a replacement PC bound to it carries on.
- **pos-proto and pos-core change**, so this needs the owner's review (AGENTS.md §6).
- **Phase 5 slices**, in order, with rough changed lines. A setting enters the register with the
  slice that makes the edge honour it, and the last two can each be cut:
  1. pos-print-agent, pos-edge: the kick in an agent's job, sent only to an agent that can (350).
  2. pos-proto: the model, report and day-end fields, the till's float, `terminal_device_id` (250).
  3. pos-edge: sessions per drawer from any till, `cash.shift.manage_other_till` and its grant,
     cash and kick per till, `NOT_A_TILL`, the boundary, `GET /api/shifts`, `shift.drawer_model`
     (850).
  4. pos-edge: `shift.close_report` and `shift.drawer_day_end` (300).
  5. ui: the Shift screen per drawer, starting and closing several, the flags, en and vi (550).
  6. pos-cloud, store-postgres, dashboard: the rollup and X/Z per drawer, the till's float (450).
  7. Assigned drawers: `shift.drawer_access`, the permission and its grant, the refusal and
     override at the till, the console's notice (550).
  8. A variance reason: `shift.variance_reason_minor`, the reason action, the till's step (300).

## The owner's answers (2026-10-07)

> "Làm theo best practice chuẩn của thế giới đi và flexible configuration được chứ không phải
> hardcode": follow the world's standard practice, and configure each policy choice.

1. One cashier on two tills: cash goes into each till's drawer under `shift.drawer_access`'s
   default, `DRAWER_ACCESS_SHARED`; `DRAWER_ACCESS_ASSIGNED` keeps a person to their own drawer.
2. A drawer still open at the day's end: `shift.drawer_day_end`, default `DRAWER_DAY_END_FLAG`.
3. Who counts the drawers: role permissions, `cash.shift.close` for a till's own drawer and
   `cash.shift.manage_other_till` for another till's, one or all, from any till.
4. The shift report: `shift.close_report`, default `CLOSE_REPORT_PER_DRAWER`.
5. Each drawer's figures in the console: under `console.reports.revenue`, as the store's are.
6. Drawers behind POS Station: the best-practice rule that every drawer springs by itself, so the
   agent's kick is slice 1 and `PER_TERMINAL` is offered from the release that ships the rest.

Approved in full on 2026-10-07 ("đồng ý làm hết"), with the amendments above: ADR-0160 decision 2
no longer offers `PER_CASHIER`, and the last condition of ADR-0165 decision 4 is lifted for a print
agent that carries the kick.
