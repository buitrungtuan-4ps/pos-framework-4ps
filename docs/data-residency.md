# Data residency: where the data rests, and when it crosses a border

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-10-28

> **This is not legal advice.** This page describes what the code does: where each class of data
> rests and when it leaves the country of the store it belongs to. Whether a given flow is a
> cross-border transfer in law, and what that law then requires (consent, an assessment, a filing, a
> contract, local storage), is for the operator's own legal counsel to decide, per country. **Whoever
> deploys this framework is responsible for complying with the law where they operate**: Vietnam's
> PDPD (Decree 13/2023/NĐ-CP), Japan's APPI, India's DPDP Act 2023, the GDPR and any other that
> applies. The framework gives mechanisms; it never decides a policy
> ([ADR-0114](adr/0114-region-is-required-recorded-visible.md)).

## The model in one paragraph

A **cell** is one complete cloud deployment ([`architecture.md`](architecture.md) §7,
[ADR-0179](adr/0179-cells-the-directory-and-the-front-door.md)). A store sells from its own machine
and needs no cloud to take money, so a cell's location changes where data rests, not how fast a till
works. One cell may serve stores in several countries: the cloud builds every country module by
default, and each store runs its own country's rules. A store whose country differs from its cell's
home country is **warned about and never refused**; a person with `console.stores.manage` answers
the warning with a reason, recorded in the audit trail. Running one cell per country, so that
personal data stays in its country, is an operator's choice the framework supports; it is not
enforced.

## What the data is, where it rests, and when it crosses a border

Drawn from the code as of the date above. "Crosses" means the data leaves the store's country under
the configuration described.

| Data | Personal? | Where it rests | Crosses a border when |
|---|---|---|---|
| Sales events (outbox, event log) | No, by construction: a name in an event payload is a compile error (`crates/pos-proto/src/pii.rs`); events carry a `subject_id` | Store SQLite, then the cell's PostgreSQL and NATS | The cell runs outside the store's country |
| Buyer record for a B2B invoice (name, tax code, address) | Yes | The store's `subjects` table only; never published to the cloud ([ADR-0107](adr/0107-the-buyer-is-a-subject.md)) | Only inside a sealed store archive (below) |
| Cloud subject records (marketplace or delivery contact) | Yes | The cell's `subjects` table, with the masking sweep and the request tool ([ADR-0076](adr/0076-subject-request-tooling.md)) | The cell runs outside the store's country |
| Employees (name, staff code) and PIN hashes | Yes | The cell; the config pull sends id, code, permissions and the Argon2id PIN hash to the store | The cell, or a hosted edge, runs outside the store's country |
| Console admins (email) and the audit trail | Yes | The cell's PostgreSQL | The cell runs outside the admin's country |
| Store archive (the whole store database) | Yes, sealed (XChaCha20-Poly1305) | The cell's object store, and the off-box backup tier ([ADR-0124](adr/0124-a-store-that-can-be-restored.md)) | The object store or the off-box destination is abroad |
| Cloud database dump | Yes | The off-box destination; each backup records the country it rests in (`BACKUP_COUNTRY`) and warns when that is not the cell's home country | The destination is abroad |
| Logs | Must not be: logging PII is forbidden ([`AGENTS.md`](../AGENTS.md) §2) | Store disk; the cell's Docker logs | A fork ships logs abroad |
| Metrics and alerts | No | Optional metrics host; one alert webhook URL | The metrics host or the alert URL is abroad |
| Reports, rollups and CSV export | No: counts and money only | The cell; a CSV goes to the admin's browser | An admin downloads from abroad |
| Tenant webhooks and the `/v1/events` feed | No: they carry the event log | Any HTTPS destination the tenant registers ([ADR-0032](adr/0032-webhooks.md)) | The destination is abroad |
| Courier booking (recipient name, phone, address) | Yes | Sent from the cell to the courier, a processor; each vendor connection records the country its vendor processes data in (`processing_country`) and flags one outside the cell's | The courier processes abroad |
| E-invoice submission (buyer fields) | Yes | Sent from the cell to the e-invoice provider ([ADR-0156](adr/0156-a-store-issues-its-e-invoice-and-the-cloud-submits-it.md)) | The provider processes abroad |
| Hosted edge placement (the whole store) | Yes | Wherever the admin chose; the region is recorded and shown beside the store's country ([ADR-0114](adr/0114-region-is-required-recorded-visible.md)) | The region differs from the store's country |
| HQ reporting across cells | No: only daily aggregates cross a cell boundary, never personal data | Each cell; HQ reads rollups through `/v1` | Always, by design, and only aggregates |

## The mechanisms the framework gives

- **A cell knows its country.** The `[cell]` section of `cloud.toml` names the cell's home country
  and the country its servers run in; the console's Fleet screen warns when they differ.
- **A store in another country is visible.** The store hub shows the warning and records who
  accepted it, why, and when (`store.cell_country.acknowledge`).
- **Backups and vendors say where they are.** `BACKUP_COUNTRY` and a vendor connection's
  `processing_country` are recorded and flagged when they differ from the cell's home country.
- **Personal data is kept out of the event stream** by the compiler, and subject requests (access,
  erasure) are served by the request tool, including in backups
  ([`guides/data-subject-requests.md`](guides/data-subject-requests.md)).
- **More cells are possible.** Each cell is one VPS ([`deploy/README.md`](../deploy/README.md)); an
  activation code names the cell that issued it, and a wrong cell answers with the right one's
  address ([ADR-0181](adr/0181-the-activation-code-names-its-cell.md)).

## Multi-cell: what is built, and what waits for a second cell

Everything a single cell needs is built. The pieces below are needed only once a second cell runs in
production, and are deliberately **not built until then**, so that they are designed against a real
need rather than a guess ([`design-principles.md`](design-principles.md), YAGNI):

| Waits for | Trigger |
|---|---|
| A store moving its cell by a signed notice ([ADR-0182](adr/0182-a-signed-cell-moved-config-node.md), C9) | A tenant must move between two live cells |
| A store finding its cell from the code's prefix by itself (today the technician points it at the right cloud) | Mis-pointed activations become a real support cost |
| One sign-in for the consoles of every cell | People administer more than one live cell |
| Master data (menus, tax rules, roles) authored once for several cells | The same catalogue must be published to more than one live cell |
| One deploy workflow over every cell, in canary order | A second cell is deployed |

Each of these starts with an ADR when its trigger is met.
