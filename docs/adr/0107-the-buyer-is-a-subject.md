# ADR-0107 — The buyer on a tax invoice is a subject, not a string

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-05
· Completes [ADR-0106](0106-the-store-is-a-legal-person.md) · Governed by
[ADR-0076](0076-subject-request-tooling.md), [`pos-spec.md`](../pos-spec.md) §15
· Relates to [ADR-0104](0104-multi-component-and-inclusive-tax.md),
[ADR-0105](0105-a-country-pack-is-values.md)

## The problem

[ADR-0106](0106-the-store-is-a-legal-person.md) put the **seller** on the receipt and named the
buyer as the next slice. A Japanese qualified invoice issued to a corporate customer must carry that
customer's name and 登録番号, and an Indian tax invoice under Rule 46 must carry the buyer's name,
address and GSTIN — without them the buyer cannot claim input tax, which is the reason a business
asks for the document at all.

The obvious shape is two more fields on `billing.bill.settled`. **The framework forbids it, and is
right to.**

`pos_proto::pii` makes `NoPii` a sealed marker trait and deliberately does not implement it for
`String`, so a name in a payload is a compile error with an instruction attached. A field-name
deny-list already rejects `name`, `address` and `phone`. `docs/pos-spec.md` §15 calls this a
**mandatory design consequence**: the event log is immutable, so anything personal inside it could
never be erased — not from a store's retention window, not from an archived partition, not from a
backup.

### The buyer's tax code is personal data often enough that it must always be treated so

For a company it is business data. For a **sole trader** it is not, and sole traders are ordinary B2B
counterparties in both markets:

- Japan's 登録番号 for a sole proprietor is issued to the individual.
- India's GSTIN embeds a PAN, which for a proprietor is a personal identifier.

A framework that stored it as business data would be wrong for a large fraction of real invoices, and
wrong in the direction that cannot be undone.

## The decision

**The settled event carries a `SubjectId` and nothing else. The buyer's record lives beside the log,
where erasing one person is deleting one row.**

```rust
BillingBillSettled {
    // …bill_id, receipt_number, subtotal, … as before…
    /// The buyer this invoice was issued to, for a B2B tax invoice.
    buyer_subject_id: Option<SubjectId>,
}
```

`SubjectId` is in `pii.rs`'s admissible list "precisely because it is the sanctioned way" to reference
a person from an immutable record. The field is `Option`, `#[serde(default)]`, and absent on every
ordinary retail sale — which is the overwhelming majority of bills and must stay exactly as cheap as
it was.

### Where the record lives, and why the store holds it

The buyer's name, tax code and address go in a **store-local** record keyed by that `SubjectId`,
written in the same transaction as the settle.

The store, not the cloud, because the receipt is composed and printed **at the till, at settle time**,
and [ADR-0001](0001-offline-first-store-autonomy.md) says a store sells with no internet. A buyer
record the till had to fetch from the cloud would make a compliant invoice depend on a link — which
is precisely the dependency this framework exists to refuse.

### That record needs a port, and it is the twentieth

`pos_proto::pii` has promised a place personal data lives since P1, `docs/pos-spec.md` §15 calls it
the **subject store**, and [ADR-0035](0035-retention-and-pii-masking.md) says what happens to it over
time. Cloud-side it exists — a `subjects` table, a masking sweep, the ADR-0076 request tooling. **On
a store there has never been one**, because until now nothing at a till held personal data.

So this ADR adds [`SubjectStore`](../../crates/pos-ports/src/subject_store.rs) as the twentieth port,
and it is deliberately *not* a buyer-shaped one. A port named `BuyerStore` would have to be joined by
a second the first time a delivery contact or a marketplace customer needs the same treatment, and
then there would be two places personal data lives — which is the arrangement §15 exists to prevent.
The port is therefore the general one: **an opaque field map, keyed by `SubjectId`, with a masking
sweep**. The buyer is its first writer, not its shape.

Three operations, and each earns its place:

- **`record` buffers into the caller's transaction**, exactly as
  [`IntakeLedger`](../../crates/pos-ports/src/intake_ledger.rs) does (ADR-0064). A settle that
  commits without its buyer record would print a compliant invoice and keep no evidence of who it was
  for; a buyer record that commits without its settle would hold a person's tax code for a sale that
  never happened. Both are wrong, and one transaction is what makes both impossible.
- **`fetch`** reads one subject by id, per subject and never in bulk.
- **`mask_before`** is the retention sweep: it replaces every field *value* with `[REDACTED]`, stamps
  `masked_at`, and keeps the `subject_id` and `collected_at`. One-way and idempotent, the same
  contract the cloud's sweep already honours.

There is deliberately **no list-every-subject read**. A port that could enumerate personal data is a
port that can export it, and nothing in this framework has a reason to.

### Retention runs at the till, on the store's own clock

`mask_before` is driven by an edge loop against the store's published `default_retention_days` — the
value a `LocalePack` has carried since ADR-0027 and which, until this slice, no edge read. A store
that has synced no locale keeps the country pack's default rather than keeping data forever.

The store masks its **own** records because it is the only party that holds them, and because
ADR-0001's store sells with no internet: a retention obligation that only discharges while the link
is up is not a retention obligation. Propagating a *cloud-originated* erasure request down to the
stores that hold a subject — the ADR-0076 tool reaching past the cloud's own `subjects` table — needs
a wire that does not exist yet, and is named here as the follow-up rather than implied by silence.

### What this buys, stated as the property it guarantees

Erasing a buyer is deleting one row, and **the financial figures still reconcile afterwards**. The
settled event keeps its subtotal, its tax lines and its total; the invoice simply no longer resolves
a buyer. That is the one technical guarantee `pii.rs` makes about personal data, and this is the first
place a *tax document* has had to honour it.

### The operator remains the data controller

The framework makes no legal judgement here and cannot. Recording a buyer's tax identity is
processing personal data, and the operator owes the lawful basis, the retention period, the notice
and — where the volume warrants — the assessment. Three things follow, and each is deliberate:

- **The field is optional and off by default.** A store that never issues a B2B invoice never records
  a buyer, so the framework does not create a processing activity nobody asked for.
- **Retention is the store's published `default_retention_days`**, the same knob every other personal
  record already answers to ([ADR-0076](0076-subject-request-tooling.md)) — not a second, invisible schedule.
- **Erasure is masking, on the store's own retention clock**, and it is the same one-way
  `[REDACTED]` the cloud's sweep applies ([ADR-0035](0035-retention-and-pii-masking.md)) — not a
  second convention. The cloud-side subject-request tooling
  ([ADR-0076](0076-subject-request-tooling.md)) keeps covering the subjects the cloud holds.

## What this deliberately does not do

- **It does not deduplicate buyers.** Two invoices to one company mint two subjects unless the
  cashier picks the same one. A cross-bill customer index is a *customer* feature with its own
  consent posture, and quietly building one out of tax invoices is exactly the kind of profiling
  `pos-spec.md` §15 exists to prevent.
- **It does not validate that the buyer's number is registered.** Format only, from the country
  module, for the reason every other tax-code check gives: a call to the authority needs a network,
  and a cashier must be able to take a corporate customer's number with the line down.
- **It does not decide India's inter-state case.** A buyer in another state makes the sale IGST at
  the full rate rather than CGST+SGST. ADR-0104 recorded that as a per-bill fact; it now has a
  per-bill place to live, and computing it is `countries/in`'s work over the `Fiscalization` port.
  Naming it here keeps its absence a decision.
- **It does not put the buyer on a kitchen ticket, a QR order, or a rollup.** One document needs it.
- **It does not sync the buyer record to the cloud, and it does not push an erasure request down to a
  store.** Both are the same missing wire — a rail for personal records, which the event stream is
  forbidden from being — and building it under a tax-invoice slice would decide its shape by accident.
  The store's own sweep discharges retention meanwhile.

## Consequences

- A Japanese or Indian store can issue a compliant B2B tax invoice, which was the last thing in this
  framework standing between those markets and a pilot.
- A buyer can be erased, and the day's takings still add up — the property that makes an immutable
  financial log and a right to erasure coexist rather than contradict.
- `billing.bill.settled` gains one optional identifier. Additive: an older consumer ignores it, and
  every bill written before this release reads back as a sale with no buyer, which is what it was.
- The framework has a store-local subject store at last, so the next kind of personal data a till
  handles — a delivery contact, a loyalty member — has a place to go that is already swept.
