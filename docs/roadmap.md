# Roadmap

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

What is being built next, in what order, and the exit criterion for each phase.
Ordering is by dependency and carries **no calendar dates** — sizes are relative:
**S** ≈ one pull request · **M** ≈ a few · **L** ≈ many · **XL** ≈ a sub-project.

Progress is tracked in GitHub milestones `P0`–`P13` plus `Track A`. This document is
the *plan*; [`CHANGELOG.md`](../CHANGELOG.md) is what actually shipped.

## Two decisions that shape everything below

**1. Framework, not a deployment.** `README.md` promises fork → set 4–6 secrets → run the
workflow. So the split of responsibility is fixed: **the framework ships the port, a fake
implementation, the contract suite, and a skeleton example. The forker supplies the
environment** — their VPS and secrets, their own signing keys, their vendor accounts, their
hardware.

Consequences, which remove almost every external blocker from the critical path:

| Concern | Framework builds | Forker supplies |
|---|---|---|
| Deployment | `bootstrap.sh`, `compose.yml`, the workflow | `VPS_HOST`, `DOMAIN`, `ACME_EMAIL`, `CF_DNS_API_TOKEN`, `RCLONE_*` |
| OTA signing | signature *verification*, `just keygen`, the rotation runbook | their own two keypairs, stored offline |
| Printers | `PrinterDriver` + an ESC/POS byte emitter with snapshot tests | real printers, verified at pilot |
| Fiscalisation | the `Fiscalization` port + `examples/fiscal-skeleton` | their country crate and provider account |
| Marketplaces, terminals, couriers, ERP | the ports + fakes passing contract tests | their vendor adapters and credentials |
| Repository governance | `CODEOWNERS`, the workflows | branch protection, the `@maintainers-*` teams, `MIRROR_REMOTE` |

`fiscal-vn` and one marketplace adapter remain in scope as the **reference** implementations,
but they land when the external decisions in Track A close — they do not gate P1–P9.

**2. Phases complete in order.** Finish P1 before starting P2, rather than cutting a thin
vertical slice through several phases to get something runnable early. Higher quality per
layer; nothing runs end to end until P5.

---

## What is already settled — do not relitigate

Rust · two binaries · SQLite WAL at the edge · PostgreSQL partitioned + RLS + rollups in
the cloud · NATS JetStream outbound-only · Garage for objects · SolidJS + Tailwind embedded
via `rust-embed` · ULID identifiers · integer money · ports-and-adapters with a CI-enforced
dependency allowlist · `snake_case` at every boundary · cloud-owned configuration ·
activation codes + single-active lease · minisign OTA in rings with two keypairs and
offline signing · monorepo, trunk-based, squash merge, ADR-before-code · proprietary
licence. Rejected and closed: Redis, Kubernetes, Kafka, ELK, ClickHouse, auto-failover,
nginx/HAProxy, path-based country routing, training mode, payment processing.

---

## Track A — start immediately, runs alongside Phase 0

These are not engineering tasks and they gate later phases, so they cannot wait their turn.

| # | Item | Blocks | Size |
|---|---|---|---|
| A1 | **Card-terminal protocol decision.** Ask the acquirer(s) whether terminals expose TCP or serial, or only a Windows DLL. Outcome decides `PaymentTerminal`'s shape and whether Linux is a supported store OS at all. | P11 payment adapter; edge OS matrix | S |
| A2 | **E-invoice provider selection with the tax authority.** Pick Viettel / VNPT / MISA, and agree the business rules — number-range allocation, catch-up submission deadlines, adjustment and cancellation flows — *before* writing `fiscal-vn`. Legal basis: NĐ 123/2020 and NĐ 70/2025. | P10 entirely | M |
| A3 | **ShopeeFood channel decision** — direct API or aggregator. | P11 vendor adapter | S |
| A4 | **WAL-shipping-on-Windows spike.** Litestream against SQLite WAL on Windows, with the archive's deliberately cruel protocol: pull mains power mid-sale, kill the process, flaky network, run two weeks continuously, then restore and **reconcile every single bill**. Pass ⇒ keep Litestream; fail ⇒ write our own — and the test suite already exists to validate it. | P9 backup/replacement; the 5–10 min swap promise | M |
| A5 | **Hardware procurement and matrix.** ESC/POS printers across brands (incl. a drawer-kick model, USB), a card terminal, a store mini-PC, a KDS panel, tablets. Long lead time; hardware tests are the one layer that needs a human. | P4 printer adapter, P13 pilot | S |
| A6 | **Data-protection posture, written down.** VN resident PII does flow through the system despite there being no CRM — Grab order name/phone/address, invoice buyer details. Confirm lawful basis under PDPD (Decree 13/2023), the VPS-in-Vietnam requirement, the retention/masking period (a config value, and the cron that enforces it is currently *unbuilt*), and record that **no employee-behaviour monitoring feature is to be designed** — telemetry is machine data only. | P7 retention job, P10 buyer fields | S |

---

## Documentation debt to clear while landing the doc set (Phase 0)

The uploaded set is not internally consistent. Fix these in the same PR that lands it —
the archive's own README says a rule present only in Vietnamese is *a bug in the English
set*.

| # | Problem | Resolution |
|---|---|---|
| D1 | `LICENSE` is referenced by `README.md` and ADR-0009 but **does not exist**. | Write it: exclusive copyright, internal use. Add the per-file header rule. |
| D2 | Every document is missing the mandatory `Status` / `Owner` / `Last reviewed` header that `engineering-guide.md` §12b requires. | Add to all 14 + the ADRs. |
| D3 | `engineering-guide.md` §8's ADR table stops at 0009; the repo has 0010–0012. | Extend the table. |
| D4 | **The port list is incomplete.** ADR-0006 names 15 ports and `architecture.md` §5 tabulates the same set, but **both omit `OrderIn`** — which ADR-0012 and `pos-spec.md` §13 explicitly depend on, and which is the reason QR ordering costs almost nothing. | The real count is **16**. Amend `architecture.md` §5 and supersede ADR-0006 with a corrected list. |
| D5 | **Hostname model contradiction.** ADR-0011 says the country lives in the hostname with a slug→country directory issuing 301s. The archive (`kien-truc` §16.4) specifies flat per-tenant names with **no visible country label**, per-tenant DNS records created through the Cloudflare API, and **DNS itself as the global slug-uniqueness ledger** (no shared database between cells). Both agree on *redirect, never proxy*; the implementation work differs completely. | **Blocks tenant provisioning (P7).** New ADR deciding it. Also record the archive-only cert note: above ~5 cells, stagger wildcard renewals to stay under Let's Encrypt's duplicate-certificate ceiling. |
| D6 | **VAT model contradiction inside one document.** Archive §4 specifies a flat store-level VAT rate with an inclusive/exclusive flag; §23.1 replaces it with **`tax_class` per item plus a channel-keyed rate table in the locale pack** (the worked example is Japan: takeaway 8%, dine-in 10% for the same item). §23.1 supersedes. English `pos-spec.md` only alludes to "tax class and rate". | Put `tax_class` + channel-keyed rates in the schema **from day one** even though VN v1 populates a single rate. Retrofitting it is a migration across every line ever written. Document it in `pos-spec.md` and the config key `store.tax.tax_class_rates`. |
| D7 | **Events partition strategy is ambiguous**: ADR-0008 says partition by `store_id`, but the only naming example is `events_p_2026_08` (monthly) and retention is "archive old partitions". | ADR before the cloud schema. |
| D8 | Capability flags appear as bare names (`tables`, `tips`) in the archive and `_enabled`-suffixed in English. | `_enabled` wins — it is the naming standard's own rule. |
| D9 | `MAINTAINERS.md` is entirely `_TBD_`, and `CODEOWNERS` routes to `@maintainers-domain` / `-architecture` / `-cloud` / `-security` teams that must actually exist in the org or **review protection silently does not apply**. | Create the teams and fill the file. Blocks branch protection being real. |
| D10 | Genuinely unspecified, and someone will assume they exist: X/Z report semantics (is an X read non-resetting? is a Z print a one-time immutable artefact?) · product-mix, tax and labour-vs-sales reports · whether service charge is taxable · tip-pool/tip-out calculation · the QR guest-page UI · accessibility beyond contrast and touch size (no keyboard navigation, focus order or screen-reader requirement exists anywhere) · `PROTOCOL_VERSION` negotiation mechanics · outbox table structure, cursor and ack protocol, NATS subject/stream naming · config delta and snapshot formats and the value of *K* ("more than K versions behind ⇒ full snapshot") · lease protocol details · the `subject_id` PII side-table schema · rollup table definitions. | Each becomes a spec issue, resolved in the phase that needs it — **not** silently invented at implementation time. |

Also harvest the archive-only rules that never reached English and are cheap to lose:
the concrete default permission matrix; **one table = exactly one open order**; **one open
shift per cashier device**; the takeaway queue number **resets daily** (distinct from the
store-lifetime receipt counter); VND quick-cash denominations 50k/100k/200k/exact; the
`:split` and `:redeliver` custom methods; `DOMAIN=<vps-ip>.sslip.io` for HTTPS with no
purchased domain; the six-item minimum alert set (incl. **invoice number range nearly
exhausted**); why a drawer-attached printer must be USB (port 9100 has no authentication
at all, and that includes the drawer-kick command); store log retention 7–14 days with a
remote "last 30 minutes" tail over NATS; the `.pre-update` database copy; the weekly
restore drill covering **both** a random store backup *and* the cloud database; the four
unequal backup classes; and the "monitoring profile off below ~50 stores" posture with its
sampling numbers.

---

## ADRs required before the code they govern

ADR-before-code is the one heavy process rule active from commit one. Beyond transcribing
0001–0012, these are new and each blocks a phase:

| ADR | Decision | Blocks | State |
|---|---|---|---|
| 0013 | **Async strategy across the boundary** — sans-I/O synchronous `pos-core` vs async ports; native `async fn` in trait vs `async-trait`; how multi-vendor families dispatch when AFIT blocks `dyn`. | P2 | **Merged** |
| 0014 | Date/time and timezone library (business-date derivation, DST at the cutoff hour). | P1 | **Merged** |
| 0015 | SQLite access strategy — `rusqlite` + a dedicated single-writer thread vs `sqlx`. | P4 | Open |
| 0016 | PostgreSQL access crate — `sqlx` (compile-time checked, needs a build-time DB or offline cache) vs `tokio-postgres` + pool. | P7 | Open |
| 0017 | Migration tooling for both tiers, plus how additive-only is enforced. | P4 | Open |
| 0018 | HTTP/WebSocket stack (axum + tower) and UI embedding. | P5 | Open |
| 0019 | OpenAPI generation from code (generated, never transcribed). | P7 | Open |
| 0020 | i18n runtime — ICU MessageFormat implementation and CLDR plural data. | P3 | Open |
| 0021 | Corrected 16-port list, superseding ADR-0006 (see D4). | P2 | **Merged** |
| 0022 | Events partition strategy (see D7). | P7 | Open |
| 0025 | Receipt-number authority as configuration, not a fixed guarantee. | P3 | Open |
| 0026 | Port shapes — one `PortError`, `Transactional`/`TxContext`, outbox cursor ordering, fault injection on the harness, and three corrections to ADR-0013. | P2 | **Merged** |
| 0023 | ~~Tenant hostname and slug-uniqueness model~~ — **resolved without a new ADR.** ADR-0011 is Accepted and canonical; the archive is frozen and explicitly non-authoritative, so ADR-0011's mechanism stands. | P7 | **Closed** |
| 0024 | `PROTOCOL_VERSION` negotiation — where the version rides on the wire, and reject behaviour. | P1 | **Merged** |

---

## Phase plan

Sizes are relative: **S** ≈ one PR · **M** ≈ a few · **L** ≈ many · **XL** ≈ a
sub-project. Ordering is by dependency; the parallel opportunities are called out.

### Stage I — Foundation

#### P0 · Rules before code — *L*
Nothing here is product code, and every item becomes a retrofit if deferred.

- Land the whole doc set (`docs/**`, ADRs 0001–0012, `AGENTS.md`, `CONTRIBUTING.md`,
  `CHANGELOG.md`, `SECURITY.md`, `MAINTAINERS.md`, `CODEOWNERS`, `.github/` templates) with
  D1–D4, D8 applied and D5–D7, D10 opened as issues.
- Keep `vietnamese-design-archive/` committed as a frozen archive, its README intact.
- Cargo workspace with all crate directories present but empty, boundaries correct,
  `[workspace.lints]` graded by layer; `rust-toolchain.toml`, `rustfmt.toml`, `clippy.toml`,
  `.gitignore` (blocks `*.key`, `.env`, `config.toml`), `.editorconfig`.
- **Pick and state the MSRV.** No document sets one — the `1.83` in `CHANGELOG.md` is part of
  a worked example, not a decision, and this machine runs 1.94.1. Release notes and
  `CONTRIBUTING.md` both promise a stated MSRV that rises only in a minor version, so it has
  to be chosen here and pinned in `rust-toolchain.toml`.
- `deny.toml`, `justfile`, and the `xtask` crate carrying the custom checks.
- CI: PR workflow under 10 minutes · merge-to-main (integration, contract, simulator smoke)
  · nightly (soak, restore drill, `cargo-audit`) · daily mirror to a second remote. Every
  action pinned to a commit SHA.
- Branch protection on `main` and `release/*`; labels including `ai-assisted`; the
  CODEOWNERS teams actually created (D9).
- `docs/roadmap.md` — this plan, as the tracked artefact.

**Exit:** `just preflight` is green on an empty workspace; adding `tokio` to `pos-core`
**fails CI** and there is a committed negative test proving the check fires; a PR touching
`crates/**` without a `CHANGELOG.md` entry or an explicit `changelog: none` reason is
blocked.

**Human-only, cannot be delegated to an agent:** generating the two minisign keypairs and
storing them per `architecture.md` §4 (offline USB + sealed paper, two copies, two
locations), writing the key-rotation runbook, configuring branch protection and the mirror
remote, and creating the org teams. Agents run without secrets and never touch signing keys
or release workflows.

### Stage II — Domain

#### P1 · `pos-proto` — the wire language — *M*
`PROTOCOL_VERSION` and its negotiation (ADR-0024) · the envelope **including
`business_date`** (the archive's envelope omits it; English is correct) · `Money` as
`currency_code` + integer `amount_minor` · in-house ULID (format only — the CSPRNG stays a
vetted crate, and generation itself lives behind `IdGenerator`) · all **38** event types
from the catalogue as typed payloads with dotted `event_type` renames · the AIP-193 error envelope · enums with
`*_UNSPECIFIED` and unknown-value tolerance on receive · forward-compatible handling of
unknown event types without data loss · the **PII-never-in-payload** rule made mechanically
hard, not just documented.

**Exit:** event-schema snapshot committed; round-trip property tests; an unknown enum value
deserialises to `*_UNSPECIFIED` instead of failing; the naming linter passes over the event
registry; a CI test proves the last two protocol versions are both understood.

#### P2 · `pos-ports` + contract harness + fakes — *L*
All 16 ports (ADR-0021), small and role-shaped. The contract suites are the deliverable
that makes "swappable" verified rather than claimed — `EventStore`'s stated contract is
ordered read-back, idempotency by ULID, and **survival of a crash mid-transaction**. Build
`EventStore`, `MessageLink`, `PrinterDriver` first. `TxContext` must make writing an event
outside a transaction *unwritable*, not merely reviewable. Ship an in-memory fakes crate
that itself passes every suite — it is what lets the domain suite run in milliseconds and
what `examples/minimal-edge` is built from.

**Exit:** every port has a contract suite; the fakes crate passes all of them; the
dependency allowlist test still green; ADR-0013 merged.

#### P3 · `pos-core` — the domain — *XL, and the largest block in the project*
The archive is blunt that the biggest lump of work is not infrastructure but table-service
business logic. This phase is where the product lives.

Money and rounding through **one** central function · business date from the store's cutoff
hour, in the **store's** timezone (computing daily rollups in the server's timezone is
named as *the* classic revenue-skewing bug) · the explicit `state × event → new state`
tables with invariants for **Order, Bill, Shift, Table**, enumerable at runtime so the
documentation table is generated and property tests bind to it · the permission registry
where adding a permission **forces a compile error in every role template** · deny by
default · a single `require(permission)` · `CapabilityContext` as the one flag read point
with cloud-side inter-flag validation · the campaign engine with its fixed evaluation order
and its split timing (item and combo at line-add; bill-level and voucher at payment start)
· *rules run offline, uniqueness runs online* · billing maths, splits by item/N/seat,
comp ≠ discount ≠ void ≠ refund · gapless per-store receipt numbering inside the bill
transaction, never conflated with a legal invoice number · inventory with per-item **and
per-modifier** BOM, consumption at fire, waste on void-after-fire, `available =
floor(min(stock/recipe))` with shared-ingredient propagation and auto-86 · the five
stock-ledger entry kinds with stocktake deltas computed against the projection at count
time · append-command order writes with same-line last-writer-wins retaining both versions
· blind shift close · translation keys with ICU plurals and `en` as the always-present
fallback.

Property tests against all four data-correctness laws. Good fixtures already exist in the
archive: the shared-ingredient arithmetic (C=10, D=8, E=6 — cooking one A drops B from 8 to
7 without B being sold), the 200-item/20-ingredient/~100-division sizing, and the promotion
examples with real prices.

**Exit:** the four laws property-tested including `sum(splits) == original_total` asserted
in CI; the state tables exhaustively tested with no reachable undefined state and no panic
path; permission catalogue snapshot committed and the matrix generated; zero I/O
dependencies; the whole suite runs in seconds with fakes.

### Stage III — The store sells

#### P4 · Edge schema and adapters — *L*
SQLite schema (undesigned — see D10): orders, order_lines, bills with
`uq_bills_store_id_receipt_number`, bill_payments, shifts, stock_ledger_entries, the
**outbox**, the config snapshot, the receipt counter, and pre-built flat read tables so the
rule "exactly one query per screen" holds. WAL mode, `synchronous=NORMAL`, `busy_timeout`,
covering indexes, 90-day retention, a WAL-size watchdog. `store-sqlite` implementing
`EventStore` + `ConfigStore`. `printer-escpos` with the retry queue, backup-printer
failover, and **bitmap rendering for any line outside the printer's code page** — without
it Vietnamese diacritics and CJK print as garbage.

**Exit:** both adapters pass their contract suites including crash-mid-transaction;
migrations proven additive-only by CI; receipt numbers gapless under concurrent load; a
real printer prints correct Vietnamese (needs A5 hardware).

#### P5 · `pos_edge` binary — *L*
axum HTTP + WebSocket fan-out under 50 ms · `rust-embed` UI · mDNS `pos.local` plus the
pairing paths that actually work in the field (a QR carrying a raw-IP link, because Chrome
on Android does not resolve mDNS and in-browser cameras need HTTPS; a DHCP reservation
pins the IP; and a manual `IP:port` + 6-digit code fallback on every client) · offline PIN
auth against synced hashes with the 5-failure/5-minute lockout enforced locally ·
`ClockSource` over SNTP with drift alarms · ULID `IdGenerator` · config hot reload under
one second with last-known-good retention · bounded channels everywhere · `tracing` with no
PII · Windows service and systemd unit · `examples/minimal-edge` running entirely on fakes,
no hardware.

**Exit:** `just run-edge`, then open a table, add lines from two devices concurrently, fire
by course, print, split, settle, print a receipt — **with the network cable unplugged the
whole time**. Kill the process mid-sale and lose only the uncommitted transaction.

#### P6 · UI — *XL*
One design-token file (4 px spacing scale, 48 px touch minimum and 56–64 px for money,
the 12/14/16/20/28/40+ type scale, one 8 px radius, one 1 px border, 100–150 ms motion,
light/dark with KDS defaulting to dark, colour tokens separate from structural tokens) ·
**four device layouts, not one that stretches**: POS terminal two-column, tablet grid with
a sliding bill, phone single-column with the primary action in thumb reach, KDS legible at
two metres · floor plan, order, cashier, KDS, expo, Today, shift, pairing · the persistent
status bar where offline is a normal working state · optimistic updates with skeletons ·
in-place PIN entry that never navigates away · the payment-screen soft lock showing who is
paying · every degraded state from the matrix having a visible exit · ICU i18n with layouts
surviving text 30% longer than English.

**Exit:** no hardcoded user-visible strings (CI-enforced); WCAG AA contrast and no
meaning-by-colour-alone; primary controls provably never move between states; a new
employee completes a full sale within five minutes without training, observed rather than
surveyed.

*P6 can run in parallel with P7 once P5's API surface is stable.*

### Stage IV — The chain

#### P7 · Cloud schema, adapters and `pos_cloud` — *XL*
`store-postgres` (partitioning per ADR-0022, RLS, JSONB, rollups — rollup definitions are
undesigned) · `link-nats` (JetStream with `max_age`/`max_bytes` per stream and an 80% alert)
· `blob-garage` — deliberately thin, since in-house WAL shipping is planned to delete this
port outright · `metrics-vm`, with the monitoring profile off below ~50 stores in favour of
sparse sampling straight into PostgreSQL.

`pos_cloud`: Argon2 + mandatory-TOTP super-admin, scoped per-tenant API keys, per-subdomain
session cookies (**never** on the parent domain — the single worst multi-tenant isolation
failure) · the four-level Tenant→Brand→Store→Device config tree with versioning, validation
that rejects bad versions and keeps last-good, delta publishing, and full-snapshot fallback
past *K* versions behind · printer/KDS *discover→propose→admin-approves* flow · idempotent
ingest keyed on ULID feeding rollups · public `/v1` API with generated OpenAPI · webhooks as
a **cursor over the event log** with HMAC, ±5-minute replay window, per-endpoint isolation,
circuit breaker, 24-hour auto-disable, and mandatory SSRF protection · the cursor feed ·
nightly reconciliation emitting the list of missing IDs to re-push · reset-cursor-and-replay
so the cloud can be rebuilt from the edges · the image pipeline (≤30 KB thumbnail, ≤150 KB
detail) · the translation grid · the retention/PII-masking cron (A6 — currently unbuilt
anywhere) · the dashboard screens, including the eight-item backlog the archive enumerates.

**Exit:** ingest idempotency and cross-tenant RLS both proven by tests rather than
convention; dashboards answer from rollups under 10 ms; a dead webhook endpoint falls
behind without any memory growth; OpenAPI drift check green.

#### P8 · Fork-and-deploy — *L*
`deploy/compose.yml` (images pinned by digest, log size and file caps, ~1.2–1.5 GB across
four containers), `Caddyfile`, idempotent `bootstrap.sh` that **generates operational
secrets on the server** and never returns them to GitHub, the deploy workflow shipping the
image over the existing SSH channel, the `reset_admin=true` break-glass behind an
Environment, the optional `k8s/` lane. Continuous WAL archiving for PostgreSQL, backups to
Garage, an rclone second tier off-box, the weekly restore drill covering both halves.
Cloudflare rules: all records grey, DNS-01 preferred, **"Flexible" SSL forbidden outright**.

**Exit:** fork → set 4–6 secrets → Run workflow → admin UI live with a one-time setup token,
in ~15 minutes with **no command typed on the server**; re-running with an older tag is a
complete rollback.

#### P9 · Fleet, OTA and machine replacement — *L*
In-house minisign verification over a vetted Ed25519 crate, both public keys baked into the
binary, the cloud-published revocation list · rings (lab → pilot → fleet, with a 25% ring
added at scale — the docs count these three different ways, so pin it in config) with
self-test, automatic rollback and a kill switch · the `.pre-update` database copy ·
activation codes exchanged once for credentials in TPM/DPAPI or the keyring · the
single-active lease that **does not expire while offline**, revokes the old machine to
read-only, and hands the replacement a **fresh invoice number range** so even an
overlapping window cannot duplicate a legal invoice number · WAL shipping per A4's verdict.

**Exit:** the simulator proves a ring rollout, a failed self-test rolling back, and the kill
switch; a real Windows machine swap completes in 5–10 minutes with every bill reconciling.

### Stage V — Country, integrations, scale

#### P10 · `fiscal-vn` and country plumbing — *L*
Blocked on A2 and by design lands *after* the core POS runs. The `Fiscalization` port
surface is allocate-range / issue / look-up / reconcile. Pre-allocated number ranges make
offline issuance possible; the queue flushes on reconnect; **calendar date, never
`business_date`** · `buyer_name` / `buyer_tax_code` / `buyer_email` feeding corporate
invoices as PII outside the event log · locale packs (currency, timezone, date and number
formats, receipt templates, channel-keyed tax rates) · host parsing that already understands
a country label, per ADR-0023 · the "invoice range nearly exhausted" alert, which is the
system's only natural hard stop on selling.

**Exit:** an invoice issues offline against a pre-allocated range and submits successfully on
reconnect; `Fiscalization` contract tests pass; `examples/fiscal-skeleton` lets someone start
a second country from the repo alone.

#### P11 · Integration surface — *XL, parallelisable per adapter*
`OrderIn` first, since marketplaces, `POST /v1/orders` and QR ordering all reuse it — that
shared port is why QR ordering is nearly free. Then `vendor-grab` and ShopeeFood per A3
(accept/reject within the vendor's SLA, ready-for-pickup, bag labels, automatic busy-mode
when the store goes offline, the 15-minute throttle window publishing prep time back), the
`payment-*` terminal per A1 with its **unavoidable unknown-result branch** parking the bill
amber for reconciliation, `shipping-ahamove` / `shipping-grabexpress`
(`CreateDelivery`/`Cancel`/`Track` plus status callbacks becoming events), `erp-sap`, and
the QR ordering cloud module (staff confirmation **on by default**, per-table rate limits,
business-hours-only, online-only, signed `table_id`). Every adapter gets its own queue,
retries, error mailbox, circuit breaker, fake, and latency chart so the "under 300 ms for
our part" SLO has an owner. **Extract `templates/adapter-template` when writing the
third adapter, not before** — the rule of three is explicit here.

**Exit:** every adapter passes its port's contract suite; the unknown-result reconciliation
flow works end to end; a guest QR order reaches the kitchen display in 0.5–2 s.

#### P12 · Simulator and capacity validation — *L*
`pos-simulator`: virtual fleet, order load, network loss, OTA rings, nightly reconciliation.
Reproduce the published envelope so the numbers in `capacity-and-reliability.md` become
measured rather than estimated — 222 events/s sustained ingest with 500–700 bursts, 200
stores offline for a day draining ~800k events in ~9 minutes, 1,000 stores each applying a
config version in under a second, a webhook endpoint dead 24 hours then recovering with no
memory growth.

**Exit:** scenario B reproduced on target hardware; the capacity tables updated with real
figures; the soak runs nightly without leaking.

#### P13 · Pilot readiness — *M*
Vietnamese operator runbooks · the hardware matrix exercised for real, including sudden
power loss · the four user guides (start from zero, write an adapter, add a country module,
run the simulator) · the fork-to-UI checklist walked by someone who has not read the docs ·
1–3 pilot stores running **in parallel with the existing system**.

**Exit:** a pilot store trades a full day on `pos_edge` alone with every bill and invoice
reconciling, and a contributor who has never seen the codebase ships a printer adapter using
only what is in the repository.

---

## Cross-cutting discipline, every phase

Docs change in the same PR as behaviour · a `CHANGELOG.md` entry with an upgrade note when
protocol, migration, permission or a default value moves · snapshots regenerated in the same
PR · additive-only schema and protocol · Conventional Commits with a crate scope · PRs
around 400 lines, one purpose, squash merged, `ai-assisted` labelled where relevant, and
**merged by a human**.

---

## GitHub tracking

- **Milestones:** one per phase — `P0 Foundation` … `P13 Pilot readiness` — plus
  `Track A External decisions`.
- **Labels:** `ai-assisted`, `bug`, `enhancement`, `rfc`, `adr-required`, `blocked-external`,
  `spec-gap`, plus one `phase:PN` label per phase and one `crate:*` label per crate.
- **Issues:** one per work item above, each carrying its exit criterion, the spec section it
  implements (`pos-spec.md` §N — `CONTRIBUTING.md` requires the citation), and its ADR
  dependency. D5–D7 and D10 become `spec-gap` issues; A1–A6 become `blocked-external`.
  Roughly 90–120 issues; I will create them in dependency order so blockers are visible.
- `docs/roadmap.md` holds this plan and is reviewed through a PR like any other document.

---

## Verification

- **Every phase:** `just preflight` green — fmt, `clippy -D warnings`, unit tests,
  `cargo-deny`, naming lint, snapshot drift.
- **The two checks that turn architecture into law:** the dependency-rule test and
  `cargo-deny`. Both get a committed negative test proving they fire.
- **Domain:** property tests against the four data-correctness laws; the state tables
  exhaustively explored for unreachable and undefined states.
- **Adapters:** the shared contract suite per port, run against every implementation
  including the in-memory fakes.
- **Integration:** real PostgreSQL and NATS containers on merge to `main`.
- **Fleet:** the simulator for rollout, offline drain, and reconciliation; nightly soak.
- **Restore:** the weekly drill restoring a random store backup *and* the cloud database,
  then reconciling totals — a backup never restored is not a backup.
- **Hardware:** the one layer needing a human — printers, terminals, sudden power loss.
- **The two end-to-end acceptance flows** the archive spells out, as automated tests:
  dine-in (open table → two devices ordering → fire course → bump → more items → split →
  pay cash + card → gapless receipt → table cycles to clean) and takeaway/marketplace
  (create or receive → queue number → KDS → bump → pay at counter → bag label).
