# ADR-0194 — Custom data is declared, and a personal field is kept apart

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Relates to
[ADR-0035](0035-retention-and-pii-masking.md), [ADR-0076](0076-subject-request-tooling.md),
[ADR-0107](0107-the-buyer-is-a-subject.md), [ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner wants HQ to add fields to existing objects (an order, a line, a bill, a table) and to
define new object types with their own screens, such as a reservation or a locker. Every custom field
declares whether it is personal data; if it is, the system stores it apart, never in events or logs,
with a retention period and a consent step.

What exists:

- **Events cannot carry a person.** `pos_proto::pii` makes `NoPii` a sealed marker and refuses
  `String` in an event payload, so a name in an event is a compile error.
- **Personal data lives in a subject store**, keyed by `SubjectId`, as an opaque map of field name
  to value, on the edge ([ADR-0107](0107-the-buyer-is-a-subject.md), `pos_ports::subject_store`) and
  in the cloud ([ADR-0035](0035-retention-and-pii-masking.md)). It reads one subject at a time, and is
  masked by a daily sweep once past a configured retention period.
- **Rights requests** (access, export, erasure) are served per subject by the cloud's tooling
  ([ADR-0076](0076-subject-request-tooling.md)), and escalated by people, not fulfilled by software.
- **Managed lists** exist for one purpose: reason codes ([ADR-0115](0115-reason-codes-are-a-managed-list.md)).

## Options considered

1. **A free-form JSON bag on every event.** It defeats the `NoPii` guarantee and every report.
2. **A new table per object type, created by migration on publish.** A package publish would become a
   schema migration on every store.
3. **Declared types, typed values, generic events, and personal values in the subject store**
   (chosen).

## Decision

1. **Declared in the package.** A package's data part ([ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md))
   declares:
   - **custom fields** on an existing object (order, line, bill, table, shift), at most 64 per object;
   - **object types**, at most 32, each with its fields and the screens that show it;
   - **lists**, managed option lists like reason codes, at most 500 entries each.
   Every field has a stable `custom_field_id`, a label key, a type, whether it is required, and
   `personal`: `true` or `false`, which is required and has no default.
2. **Few types.** `int`, `bool`, `choice` (from a list), `date`, `quantity` (an integer with a unit),
   and `text` (at most 1 KiB). **Free text is always personal**: nobody can promise what a person
   will type into it, so `text` with `personal: false` is refused at publish. There is no money type:
   an amount that matters is a core concept with its own ADR.
3. **Non-personal values travel in events.** Two generic events, additive: `data.custom_field.set`
   (the object it is on, and typed values by field id) and `data.custom_record.changed` (an object
   type's record: its id, type and typed values). Both carry only structured, non-personal types, so
   `NoPii` holds. The edge and the cloud project them into one table each, keyed by object and field,
   so no publish needs a migration.
4. **Personal values go to the subject store.** A personal value is written to the subject store as
   a field of a `SubjectId`; the event carries only that id. The edge's and the cloud's existing
   subject stores hold it, the daily sweep masks it, and ADR-0076's tooling covers it with no change.
5. **A personal field needs a retention period and a consent step.**
   - Its declaration names a retention period in days. There is no default, as ADR-0035 requires;
     a personal field without one is refused at publish.
   - Its screen shows the consent step the package declares, and the value is saved only once the
     consent is recorded: the purpose's label key and when it was given, stored with the subject.
     The lawful basis and the consent text are the tenant's legal decision, not the software's.
   - Personal values are never visible to rules ([ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md)),
     never logged, and never sent through the gateway unless its endpoint is declared as a processor
     (ADR-0198).
6. **Schema changes are additive.** A field or type can be added, or deprecated (hidden from new
   entries, kept for reading). It is never removed, renamed or retyped, and `personal` never changes
   from `true` to `false`. A different type is a new field. The cloud refuses a package version that
   breaks this against every earlier version of the package.
7. **Reports and printing read the projection.** A report groups or filters by a non-personal field;
   a receipt or ticket template may print any field, personal ones only on the copy for the person
   they belong to, as the buyer's details print on a tax invoice today.

## Consequences accepted

- **Additions to `pos-proto`:** the data declaration types and two events, additive, with the
  owner's review; the event snapshot gains them.
- **Two projection tables**, edge and cloud, by additive migration.
- **Some fields that are not personal are treated as if they were**, because they are free text.
  This costs a consent step and a retention period, and buys a guarantee that needs no human review
  per field.
- **The tenant must decide lawful basis, consent wording and retention** for each personal field
  before it publishes it. The console asks; the software does not answer for them.
