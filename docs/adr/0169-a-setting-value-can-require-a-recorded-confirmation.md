# ADR-0169 — A setting value can require a recorded confirmation

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-07, accepted
2026-10-14 (owner delegated approval 2026-10-14)
· Extends [ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)
· Amends [ADR-0167](0167-a-till-has-its-own-cash-drawer.md) decision 9
· Relates to [ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md) decision 9 and
[ADR-0125](0125-a-release-is-one-decision-many-writes.md)

## The problem

ADR-0167 decision 9 offers `shift.drawer_access = DRAWER_ACCESS_ASSIGNED`. Under it, a drawer's
over/short is one employee's figure, which is personal data under Vietnam's Decree 13/2023 and
under any comparable law where a tenant trades. The decision has the console *say* that the tenant
needs a lawful basis and must have told its staff. That is not enough: nothing that attributes
figures to an employee should be switched on without a **confirmed** lawful basis and staff
notification, recorded against the person who confirmed it.

The settings machinery cannot express that today:

- **The register.** `pos_proto::settings::Setting` (`node`, `field`, `shape`, `scopes`, `since`,
  `summary`) cannot mark a value as needing a confirmation.
- **The console.** The Settings screen draws only from the catalogue.
- **The write.** `PUT /admin/settings` takes `{tenant_id, setting_key, scope, scope_id, value}`
  and checks the value against the register alone.
- **Other writes reach the same fields without the register.** Every one of them runs under
  `console.config.publish`, and none consults the register:
  - `PUT /admin/stores/{store_id}/config/{level}` writes a whole authored layer;
  - `POST /admin/stores/{store_id}/config/rollback` restores an earlier version;
  - a scheduled publish, and so a config release (ADR-0125), writes a node onto a store's layer
    when its moment comes.

  Any of them could switch `ASSIGNED` on with no notice at all.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Hard-code `shift.drawer_access` in the write route and the Settings screen | Breaks ADR-0160's register-driven console; the layer, rollback and scheduled writes remain a way around |
| B | A notice only, as decision 9 reads | No record that anyone confirmed anything; does not meet "confirmed" |
| C | **A register attribute: a value may require a recorded confirmation, enforced on every route that can write it** | About 200–250 production lines, in four small slices |

## Decision

Option **C**.

1. **The register.** `Setting` gains `confirm: Option<Confirm>`, where `Confirm` is
   `{ value, notice }`:
   - `value` is the wire token that needs confirming;
   - `notice` is the translation key of the sentence the person confirms,
     `settings.item.<node>.<field>.confirm`.

   The catalogue serves it, and `docs/snapshots/settings.txt` gains a `confirm=` row. A register
   test pins that **no preset is a value that needs confirming**, so creating a store never
   confirms on anyone's behalf. Nothing in the register names a country: whether a value needs
   confirming is a property of what it records, and the notice's wording is the translation's.
2. **The write.** `PUT /admin/settings` gains `notice_confirmed`, a boolean, default `false`.
   - Writing a value that its setting's `confirm` names, at any scope, without
     `notice_confirmed: true`, is refused: `400`, with the detail `notice_confirmed` /
     `CONFIRMATION_REQUIRED`.
   - The `setting.set` audit entry records `notice_confirmed: true` and the notice key beside the
     value. Like every entry, it records who and when.
   - Any other value, and clearing a setting, needs no confirmation.
3. **No way around it.** One check, in one place, guards every other write of a store's
   configuration: the authored-layer write, the rollback, and the scheduled publish, which a
   release uses. Before it saves, each compares the store's effective configuration before and
   after the write. If the write would make the store run a confirm-needing value it did not run
   before, it is refused:
   - the layer write and the rollback answer `400`, with the detail naming the setting key and
     `CONFIRMATION_REQUIRED`, and a message that points to Settings;
   - a scheduled publish, and a release, is checked against each store when it is scheduled and
     refused the same way. It is checked again when its moment comes, since the store may have
     changed meanwhile: then it is not applied, and the activator reports it as it reports a node
     that no longer validates.

   A write that leaves such a value as it was, confirmed earlier, passes, so a layer may be
   rewritten with it unchanged. The settings republish is the one writer the check trusts, because
   every value in the settings store passed decision 2 when it was written.
4. **The console.** For any value whose catalogue entry carries `confirm`, the Settings screen
   draws a checkbox with the notice, from its translations in every locale the console ships, and
   sends `notice_confirmed`. The Save stays disabled, and says why, until it is ticked. This is
   generic, not one setting's branch.
5. **The first such value** is `shift.drawer_access = DRAWER_ACCESS_ASSIGNED`. Its notice reads, in
   English: "We have a lawful basis for recording which employee is accountable for a drawer, and
   we have told our staff, as the data protection law that applies to us requires." The Vietnamese
   translation names Decree 13/2023, which is that law in Vietnam.

## Consequences accepted

- **Additive.** `PROTOCOL_VERSION` is unchanged. The catalogue gains an optional field, the write
  gains an optional field, and `settings.txt` gains one row per confirm. Nothing reaches the edge.
- **The confirmation is a record, not a legal review.** The cloud records who confirmed, when and
  which notice. It cannot check the lawful basis itself, which stays the tenant's responsibility
  under its data protection officer.
- **A confirmation covers the scope it was written at,** including a store that joins that brand
  or store group later. The person confirming is told the scope on the same screen.
- **A layer edit loses one capability:** turning on a confirm-needing value. Like everyone else,
  its author confirms it on Settings. A rollback to a version that ran such a value, from one that
  does not, needs the value confirmed on Settings first.
- **ADR-0167 decision 9 is amended**, from "the console says" to "the console requires a recorded
  confirmation". Nothing else in ADR-0167 changes.
- **Order of work.** The gate is built in four slices before ADR-0167 slice 7b lands: the register
  field; the settings write with its audit; the layer, rollback and scheduled-publish check; the
  console. `shift.drawer_access`'s register entry, with its `confirm`, lands no later than the
  edge slice that honours the field, so there is no window in which a store can be made to run
  `ASSIGNED` unconfirmed.
