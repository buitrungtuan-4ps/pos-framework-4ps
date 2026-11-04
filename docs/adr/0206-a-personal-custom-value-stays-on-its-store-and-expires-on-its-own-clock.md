# ADR-0206 — A personal custom value stays on its store and expires on its own clock

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-04
· Amends [ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md) decisions 4
and 5 · Extends [ADR-0107](0107-the-buyer-is-a-subject.md) (the `SubjectStore` port)
· Relates to [ADR-0035](0035-retention-and-pii-masking.md), [ADR-0076](0076-subject-request-tooling.md),
[ADR-0124](0124-a-store-that-can-be-restored.md), [ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)

Proposed: decisions 1, 3, 4, 5, 6, 9 and 10 wait on the owner's acceptance, because they change a
port, the handling of personal data, or what prints. Decisions 2, 7, 8 and 11 were set by the
coordinator under ADR-0194 and are recorded here, so that phase 3 builds from one record; decision 7
takes effect when this record is accepted. Until then, the cloud refuses to publish any field with
`personal: true`.

## Context

ADR-0194 puts a personal custom value in the subject store, under a `SubjectId` that the event
carries. It also says the daily sweep masks the value and that ADR-0076's tooling covers it "with no
change". Neither holds for a value that a till enters:

- **Retention.** ADR-0194 decision 5 gives each personal field its own retention period. The edge's
  sweep (`SubjectStore::mask_before`) masks every record collected before one store-wide cutoff,
  taken from `default_retention_days`. A field kept for 30 days in a store whose default is 365 would
  be held eleven months too long. A field kept for 730 days would be masked early.
- **Rights requests.** ADR-0107 keeps the store's subject records on the store and never sends them
  to the cloud. ADR-0076's lookup, export and erase read only the cloud's `subjects` table, so they
  cannot see a value entered at a till.
- **At rest.** The edge's `subjects` table (`store-sqlite` migration 0007) holds its fields as plain
  JSON. Only the store archive is sealed (ADR-0124).

## Options considered

For retention:

1. Mask at the store default and refuse a field whose period differs. This is simple, but a tenant
   could never declare a period shorter than the store's.
2. Give the sweep a period per field id. The port would then have to understand a record's field
   names, which ADR-0107 kept opaque on purpose.
3. **Record when each subject expires** (chosen).

For where personal values rest, and how a request reaches them:

1. A rail that sends personal records from the edge to the cloud, so ADR-0076 covers them unchanged.
   It is a new wire for personal data, and it is a cross-border transfer wherever the cell is outside
   the store's country.
2. Values stay on the store, and the store offers an audited read and erase by subject.
   A request then needs somebody at every store.
3. **Values stay on the store; the cloud sends erasures down by subject id, and the store answers a
   per-subject read** (chosen). A subject id is not personal data, so the instruction can travel the
   way configuration does.

## Decision

1. **A subject record may carry its own expiry.** `SubjectRecord` gains `retain_until: Option<Timestamp>`.
   This is an additive change to the `SubjectStore` port, with an additive `store-sqlite` migration
   for the column and an index on it.
   - `mask_before(cutoff, now)` masks an unmasked record when its `retain_until` is at or before
     `now`. A record without `retain_until` keeps today's rule: it is masked when `collected_at` is
     at or before `cutoff`. A B2B buyer's record (ADR-0107) does not change.
   - A personal custom value is written with `retain_until = collected_at + retention_days`, with the
     days taken from the field's declaration.
   - The contract test suite gains both cases, and the edge's daily loop does not change.
2. **One subject per record.** All the personal values of one custom record, or of one object's
   custom fields, are one subject. Publish refuses an object type, or a set of fields on one existing
   object, whose personal fields declare different retention periods. A record therefore expires
   whole, never one field at a time.
3. **Personal values stay on the store that collected them.** They are never sent to the cloud and
   never published on a wire. So there is no new cross-border flow: what [`data-residency.md`](../data-residency.md)
   says of the B2B buyer record holds for these values too. The one exception is the sealed store
   archive (ADR-0124), as for every table.
4. **An erasure reaches the store by subject id.**
   - When the owner erases a subject through ADR-0076 that the cloud does not hold, but which a
     store's events name, the cloud adds the `subject_id` to that store's new `erasures` config
     node. Only `console.subjects.manage` can do this, it is per subject, and it is audited as
     ADR-0076 decision 3 says.
   - The edge masks the subject when it applies the node. This is the same one-way, idempotent
     `[REDACTED]` masking, so a node applied twice changes nothing.
   - The edge confirms with an additive event, `data.subject.masked`, which carries the
     `subject_id` and the time and nothing else.
   - The cloud removes an id from the node once it has received that event. The node holds at most
     1,000 ids, and the console refuses an erasure that would overflow it.
   - Withdrawn consent is erased through this same path.
5. **Access is a read at the store, one subject at a time.** The edge adds `GET /api/subjects/{subject_id}`.
   It returns one subject's record and nothing else. There is no list and no search. It needs
   `data.personal.read`, and every call writes an audit entry with the actor, the subject id and the
   action, never the values. Answering an access request from the store is the Data Protection
   contact's procedure; the software only provides the read.
6. **The consent record is part of the subject.** The consent step writes
   `consent.<purpose label key>` with the time consent was given, as one more field of the subject.
   It is masked with the rest of the record: once nothing is held, nothing needs a consent behind it.
7. **Two permissions.** `data.record.write` (risk LOW) enters and changes custom records and custom
   field values. `data.personal.read` (risk HIGH) shows a personal value again after it has been
   saved, on a till or through decision 5. Entering a personal value does not need
   `data.personal.read`; reading it back does. A custom field on an existing object is set under that
   object's own edit permission together with `data.record.write`. Both permissions join the
   permission snapshot.
8. **No personal field on staff.** Publish refuses `personal: true` on a field of a `shift`, and no
   field type can reference a member of staff. A custom field must not become a record kept about an
   employee, which this framework does not keep.
9. **What prints.**
   - A personal value prints only on a document handed to the person it belongs to: the record's
     own slip, or the buyer's copy of a tax invoice.
   - It never prints on a kitchen ticket, a pre-bill left on a table, the store's copy, a shift
     report, or a reprint, unless the person holding `data.personal.read` reprints the person's own
     copy.
   - The receipt template (ADR-0207) marks which documents count as the person's copy.
10. **At rest.** A box that holds personal custom values runs with the operating system's full-disk
    encryption (BitLocker on the Windows reference box). [`go-live.md`](../go-live.md) gains this as a
    gate in the change that first writes a personal custom value. Encrypting each field with a key
    from the `KeyVault` port is a later record, if the owner wants it.
11. **Until this record is Accepted and the per-record expiry has landed,** the cloud refuses at
    publish every field declared `personal: true`, naming the field. So no personal value can be
    written before its retention and its erasure work.

## Consequences accepted

- **A port change.** `SubjectStore` gains an optional field and a second masking rule, and its
  contract tests grow to match. Both are additive: every existing record reads back without an
  expiry and is swept as before.
- **One new config node** (`erasures`), **one new event** (`data.subject.masked`), **one new edge
  route** and **two permissions.** All are additive and take their snapshots in the same change.
- **An erasure waits for the store to come online.** A store that is offline masks the subject when
  it next pulls. Until then the console shows the erasure as pending.
- **An access request may need the store to be reachable.** Values held only at the store are read
  at the store.
- **The owner's obligations**, which the software does not decide:
  - the lawful basis for each personal field under Vietnam's PDPD (Decree 13/2023/NĐ-CP), and under
    the GDPR or another law where it applies;
  - the consent wording behind each purpose's label key;
  - each field's retention period;
  - a data protection impact assessment before personal custom data is used in any report or
    analysis;
  - escalating an EU resident's rights request to the Data Protection contact.

  This record adds no cross-border flow. Choosing the sending rail (option 1) instead would need a DTA
  or explicit consent for every cell outside a store's country.
- **Phase 3 publishes through the API only** (the console's data designer is phase 4). The API's
  documentation states these obligations, and the console will repeat them when it asks.
