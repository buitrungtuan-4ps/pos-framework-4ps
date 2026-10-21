# ADR-0178 — Data residency and cross-border transfer, per country

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-10-21 · Builds on
[ADR-0011](0011-country-in-hostname.md) and [ADR-0114](0114-region-is-required-recorded-visible.md)
· Relates to [ADR-0035](0035-retention-and-pii-masking.md),
[ADR-0046](0046-backups-and-restore.md), [ADR-0076](0076-subject-request-tooling.md),
[ADR-0107](0107-the-buyer-is-a-subject.md), [ADR-0124](0124-a-store-that-can-be-restored.md),
[ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md)

> **Needs review by legal and the Data Protection contact before it can be accepted.** This record
> was drafted with AI assistance from the code and the documents in this repository. It contains no
> legal advice. Every statement about what a law requires is marked **[to confirm by legal]** and is
> a question for legal, not a finding. The record waits on those answers, and on the owner's decision
> on gate [X3](../gate-register.md).

## Context

[ADR-0011](0011-country-in-hostname.md) and [`architecture.md`](../architecture.md) §7 already give
the answer the framework was designed around: one **cell** (one complete cloud deployment) per
country, reached by hostname, so that *"personal data never crosses a border"*. Two decisions are
recorded since then:

- [Gate L6](../gate-register.md) and [`plans/standardisation.md`](../plans/standardisation.md)
  record the owner's decision of 2026-10-14: the cloud for Vietnamese stores is hosted in Vietnam.
- [ADR-0114](0114-region-is-required-recorded-visible.md) requires a region on every hosted edge
  placement. The console shows it beside the store's country and warns when the two differ.

The code does not yet make the cell rule true by construction. Five gaps:

1. `pos-cloud` builds all four country modules by default (`crates/pos-cloud/Cargo.toml`,
   `default = ["country-zz", "country-vn", "country-jp", "country-in"]`). So one deployment can
   register stores in any country, and nothing checks that a store's country matches its cell's.
2. A cell does not know which country it serves or where it runs. `cloud.toml` has no such field.
   The only `region` in it is the S3 signing input, `"garage"`, which is not a location
   (ADR-0114). Gate X3 is still open.
3. Where backups go is not recorded. `deploy/backup.sh` ships a `pg_dump`, and the store archives,
   to `RCLONE_REMOTE`, a per-machine value nothing reads (ADR-0114 names this gap).
4. Where a vendor processes data is not recorded. Connections (ADR-0153) name a provider, not a
   country.
5. Japan (APPI) and India (DPDP Act 2023) have no hosting decision. That is gate X3.

## What the data is, where it rests, and when it crosses a border

This table is drawn from the code as it is today. "Crosses" means the data leaves the store's country
under the configuration described. Whether that is a transfer in law is **[to confirm by legal]**.

| Data | Personal? | Where it rests | Crosses a border when | Code |
|---|---|---|---|---|
| Sales events (outbox, event log) | No, by construction: `pos_proto::pii` makes a name in a payload a compile error. Events carry a `subject_id`. | Store SQLite, then the cell's PostgreSQL and NATS | The cell runs outside the store's country | `crates/pos-proto/src/pii.rs`, ADR-0145 |
| Buyer record (name, tax code, address) for a B2B invoice | Yes | Store `subjects` table only. It is never published to the cloud. | It leaves the shop only inside a sealed store archive | ADR-0107, `store-sqlite` migration `0007_subjects.sql` |
| Cloud subject records (marketplace or delivery contact, buyer) | Yes | The cell's `subjects` table, with a masking sweep and the ADR-0076 request tool. No production code path writing it was found today. | The cell runs outside the store's country | `store-postgres` migration `0005_cloud_subjects.sql`, `crates/pos-cloud/src/retention/` |
| Employees (name, staff code), and PIN hashes | Yes | Cell `employees`. The config pull sends id, code, permissions and the Argon2id PIN hash to the store. | The cell, or a hosted edge, runs outside the store's country | `0023_employees.sql`, ADR-0070, ADR-0114 |
| Console admins (email) and the audit trail | Yes | Cell PostgreSQL | The cell runs outside the admin's country | `0018_cloud_admin_users.sql`, ADR-0069 |
| Store archive (whole store database, including buyer records) | Yes, sealed | Sealed on the store (XChaCha20-Poly1305). The cell's object store holds it; the off-box tier holds a copy. The store's key is wrapped under the cell box's `archive_key_secret`, which is not in the dump. | The object store or the `RCLONE_REMOTE` destination is abroad. Whether sealed data counts as a transfer is **[to confirm by legal]**. | ADR-0124 and its Amendment 1, `crates/pos-edge/src/backup.rs` |
| Cloud database dump | Yes | `RCLONE_REMOTE`, whose location is not recorded | The remote is abroad | `deploy/backup.sh`, ADR-0046 |
| Logs | Must not be: the AGENTS.md rule forbids logging PII | Store disk; cell Docker logs (`max-size`). There is no log-shipping adapter in the tree. | A fork adds log shipping abroad | `docs/architecture.md` §8 |
| Metrics and alerts | No. Labels are store, adapter, event type and status. | Optional `metrics-vm`; one deployment-wide `alert_webhook_url` | The metrics host or the alert URL is abroad | `crates/pos-cloud/src/config.rs`, ADR-0073 |
| Reports, rollups and CSV export | No. Counts and money only. | Cell PostgreSQL. A CSV goes to the admin's browser, wherever that browser is. | An admin downloads from abroad | ADR-0081 |
| Tenant webhooks and the `/v1/events` feed | No: they carry the event log | Any HTTPS destination the tenant registers | The destination is abroad | ADR-0032 |
| Courier booking (recipient name, phone, address) | Yes | Sent from the cell to the courier, which is a processor | The courier processes abroad | `crates/adapters/shipping-ahamove/src/client.rs`, ADR-0058 |
| E-invoice submission (buyer fields) | Yes | Sent from the cell's connector to the provider (Proposed, ADR-0156) | The provider processes abroad | ADR-0156 |
| Hosted edge placement (the whole store) | Yes | Wherever the admin chose; the region is recorded | The region differs from the store's country, which the console shows | ADR-0110, ADR-0114 |

## Obligations to confirm, per country

Every line here is **[to confirm by legal]**. Each one names a question; none states an answer.

**Vietnam: PDPD, Decree 13/2023/NĐ-CP.**

- Does hosting the cell in Vietnam (the 2026-10-14 decision) mean no transfer impact assessment is
  needed for data at rest? **[to confirm by legal]**
- Do these still need one, and must it be filed? **[to confirm by legal]**
  - an admin reading the console from outside Vietnam;
  - a courier or e-invoice provider processing outside Vietnam;
  - an off-box backup destination outside Vietnam.
- Is the lawful basis for each data class settled (gate L1)? This includes contract performance for
  the courier, as ADR-0058 states. **[to confirm by legal]**
- What retention period applies to each class? The `vn` module ships `default_retention_days: 365`
  as a placeholder its own comment says the operator must confirm. **[to confirm by legal]**

**Japan: APPI.**

- If a Japanese store's data rests in a cell outside Japan, what does provision to a third party in a
  foreign country require: consent, information to the person, or an equivalent-measures basis?
  **[to confirm by legal]**
- Does a cloud operated by the same company count as a third party? **[to confirm by legal]**
- Is the hosting region for Japanese stores decided (gate X3)? **[to confirm by legal]**
- Does a sole trader's 登録番号 on a qualified invoice make the buyer record personal
  information? ADR-0107 treats it as personal as a design rule, not as advice. **[to confirm by legal]**

**India: DPDP Act 2023.**

- Are transfers outside India permitted except to countries the government restricts, and is any
  such restriction in force? **[to confirm by legal]**
- Does a sector rule require Indian payment data to be stored in India, and does any of this data
  fall under it? **[to confirm by legal]**
- What notice and consent does a buyer's GSTIN, which embeds a PAN, need? **[to confirm by legal]**
- Is the hosting region for Indian stores decided (gate X3)? **[to confirm by legal]**

**EU: GDPR, if ever.** There is no EU store and no EU cell.

- Does serving an EU-resident guest at an Asian store bring GDPR into scope? **[to confirm by legal]**
- Would an EU store need its own cell, and a DPA with each processor (courier, e-invoice,
  hosting)? **[to confirm by legal]**
- Rights requests go to the Data Protection contact (gate L4, ADR-0076). The framework never
  fulfils them on its own; that is a design rule, not a legal statement.

## Options considered

**A. One cell per country, the ADR-0011 design, made checkable.**

- Each country's stores live in a cell in that country.
- Personal data at rest never leaves the country.
- The remaining flows are vendors, admin access from abroad, and backups. Each is recorded, so it
  can be cleared one at a time.
- Costs one deployment per country, which the fork-and-deploy flow already supports.

**B. One cloud, with a region chosen per tenant.**

- Cheaper to run.
- It needs data routing inside one deployment that the code does not have. PostgreSQL is
  partitioned by tenant and store (ADR-0008), not by region.
- A tenant with stores in two countries would need two regions anyway.

**C. One cloud for every country, with transfers cleared by agreements or consent.**

- The simplest operation.
- Every non-home store's data crosses a border by design. Whether each country allows that, on what
  terms, is **[to confirm by legal]**.
- It reverses ADR-0011, whose stated reason is the law.

**D. Split by data class.** Personal data stays in a per-country cell; non-personal events are
centralised.

- The event log is already free of personal data, so this is possible in principle.
- But it needs two control planes, and a join across them for every subject request.
- Option A gives the same result with one.

## Decision (proposed)

**Option A.** One cell per country, as ADR-0011 already says. This record adds four facts the cell
records, so that "data stays in the country" can be read rather than remembered.

Nothing is implemented by this record. Each item below is a later slice with its own review.

1. **The cell states what it serves.**
   - `cloud.toml` gains the country the cell serves and where it runs, written by whoever deploys it.
   - Registering a store, or publishing a `locale` node, whose country differs from the cell's is
     **warned and needs an audited acknowledgement**. It is not refused.
   - This is ADR-0114's posture for hosted edge placements: mechanism, never policy, and the
     operator stays the controller.
   - Whether a refusal would be better is the owner's call.
2. **Each backup destination is recorded.** `RCLONE_REMOTE` gets a recorded country beside it.
   This closes the gap ADR-0114 names.
3. **Each vendor connection records where the vendor processes data**, as the admin states it. The
   console shows it beside the cell's country, with the same warning.
4. **A transfer register.** A document under `docs/` lists every flow in the table above, each
   with its lawful basis and agreement. Legal and the Data Protection contact fill it in, not code.
   Gate L3 points to it.

## Consequences

- No code, protocol, port or dependency changes with this record, and `PROTOCOL_VERSION` does not
  move. Items 1–3 each need their own record or slice, and are additive.
- Gate X3 stays open. This record makes no hosting decision for Japan or India.
- Gates L1, L2, L3 and L6 stay with legal and the Data Protection contact. A recorded country is a
  fact to clear against; it is never the clearance itself.
- The `default` country feature list of `pos-cloud` stays as it is. A cell that wants to refuse
  other countries' stores at build time can drop features, but the build does not enforce that.
- Admin access from abroad, and CSV exports, stay outside what code can record. They belong in the
  transfer register.
- If legal's answers make option A unnecessary for a country, this record is superseded for that
  country. It is not edited.
