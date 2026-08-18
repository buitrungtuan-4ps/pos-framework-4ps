# Architecture

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-08-18

How the system is built, and why each choice was made. Business behaviour is in [pos-spec.md](pos-spec.md).

---

## 1. Four principles

1. **The store is autonomous.** Every sale, print, and payment completes inside the store's LAN. The cloud is for administration, synchronisation, and reporting. If the internet disappears for a week, the store keeps trading.
2. **One binary per tier.** `pos_edge` at the store, `pos_cloud` in the cloud. Both are modular monoliths: internally split into modules with clear boundaries, externally a single static executable. No microservices, no runtime to install.
3. **Machines are cattle, not pets.** A store PC is a replaceable unit. Swap the hardware, enter an activation code, and the replacement is trading in 5–10 minutes.
4. **Configuration lives in the cloud.** Menus, prices, printers, roles, tax rules, feature flags — all of it is owned centrally and pushed down. A store never holds authoritative settings.

## 2. Store tier (`pos_edge`)

Runs as a Windows service or systemd unit on an ordinary PC or mini-PC.

| Concern | Implementation |
|---|---|
| Storage | SQLite in WAL mode, in-process. No database server to install or patch on 1,000 machines. |
| UI | SolidJS bundle embedded in the binary (`rust-embed`), served over the LAN. Clients are browsers. |
| Realtime | WebSocket push to every client: new lines, kitchen bumps, sold-out flags appear in under 50 ms. |
| Devices | ESC/POS printers (USB or LAN), cash drawers, payment terminals, barcode scanners. |
| Discovery | Publishes `pos.local` via mDNS. Clients pair by scanning a QR code containing `http://<ip>/pair?...`, or by typing `IP:port` plus a 6-digit code shown on the server screen. |
| Durability | Every sale is a transaction. Events land in a local outbox table before anything is sent anywhere. |

**Order writes are append commands.** Two devices adding items to the same table produce two commands that merge; they never overwrite each other. Only edits to the *same line* apply last-writer-wins, and both versions stay in the audit log.

## 3. Cloud tier (`pos_cloud`)

Four containers on a single VPS: `pos_cloud`, PostgreSQL, NATS, Garage (S3-compatible). An optional `monitoring` profile adds VictoriaMetrics and Grafana.

| Concern | Implementation |
|---|---|
| Transport | **NATS JetStream.** Stores dial out only, which solves 4G and CGNAT without port forwarding or VPNs. Durable, disk-backed, acknowledged delivery. |
| Database | **PostgreSQL**, partitioned by `store_id`, row-level security per tenant, JSONB for flexible payloads, and **rollup tables** so every dashboard query is a small aggregate read (<10 ms). |
| Objects | **Garage** (or MinIO) for backups and OTA artifacts. |
| TLS | Caddy obtains and renews Let's Encrypt certificates. Later this is absorbed into `pos_cloud` via `rustls-acme`, removing a process. |
| Identity | Self-implemented auth: Argon2 password hashing, TOTP second factor, per-tenant API keys with scopes. |

### 3.1 Configuration tree

```
Tenant ─► Brand ─► Store ─► Device
   each level inherits from its parent and may override
```

Changes are versioned and shipped as deltas. A store applies a new version in under a second (hot reload) with no restart. If a delta cannot be applied, the store keeps the last known-good version and raises an alert. A store that has been offline too long, or has fallen too far behind, pulls a **full snapshot** instead of replaying deltas.

Employee PINs sync down as hashes so login works offline. Printers and displays follow a *store discovers → admin approves* flow: the store reports what it found on the network, and an administrator assigns roles from the dashboard.

### 3.2 Synchronisation

```
sale ──► SQLite txn + outbox ──► NATS JetStream ──► cloud ingest (idempotent by ULID)
                                                        │
                                                        ├─► store partition (raw events)
                                                        └─► rollup tables ──► dashboards
```

Identifiers are **ULIDs**: generated offline, sortable by time, collision-free when thousands of stores merge. Ingestion is idempotent, so at-least-once delivery is safe. A nightly reconciliation compares event counts and checksums between each store and the cloud; a mismatch lists the missing IDs and re-pushes them.

## 4. Fleet operations

**Provisioning.** Creating a store yields a one-time activation code. The installer exchanges it for long-lived credentials stored in the OS keystore (TPM/DPAPI on Windows, keyring on Linux). No secrets ship inside installers.

**Single-active lease.** Exactly one server may be active per store. A replacement machine takes the lease; if the old machine returns from the dead it becomes read-only. This prevents split-brain and duplicate receipt numbers.

**Over-the-air updates.** Signed with minisign. Rolled out in canary rings (for 50 stores, two rings; for 1,000, four). Each update runs a self-test after install and rolls back automatically on failure. Because a rollback of the binary requires a rollback of the data, the database file is copied to `.pre-update` before migration, and **migrations within a release may only add** — never drop or rename. A kill switch halts a rollout instantly.

**Two signing keypairs, both public keys baked into the binary.** Key A signs day-to-day releases; key B is the sealed spare. If A is lost or leaked, one release switches the fleet to B — no need to visit 1,000 machines. The cloud publishes a revocation list that edges check before installing. Private keys never live on the CI runner or the VPS: releases are signed by hand from an offline USB key.

## 5. Ports and adapters

`pos-core` never calls NATS, PostgreSQL, S3, or a printer. It calls **ports** — traits owned by this project. Every external technology is one implementation of one port.

| Port | Responsibility | Current implementation |
|---|---|---|
| `EventStore` | Append and read events, outbox | SQLite / PostgreSQL |
| `ConfigStore` | Config snapshots and deltas | SQLite / PostgreSQL |
| `MessageLink` | Durable store↔cloud channel | NATS JetStream |
| `BlobStore` | Large objects (backups, artifacts) | Garage / MinIO |
| `MetricsSink` | Numeric telemetry | VictoriaMetrics |
| `Signer` / `KeyVault` | Signature verification, key storage | minisign, OS keystore |
| `ClockSource` | Time and drift detection | SNTP |
| `IdGenerator` | ULID generation | in-house |
| `PrinterDriver` | ESC/POS, print queue | in-house |
| `PaymentTerminal` | Card terminals (FFI or TCP) | per acquirer |
| `Fiscalization` | Legal invoicing per country | `fiscal-vn` first |
| `DeliveryVendor` / `ShippingDispatch` / `ErpSink` | Marketplaces, couriers, ERP | per vendor |
| `OrderIn` | Inbound orders from outside the store | marketplaces, `POST /v1/orders`, QR ordering |

The list above is authoritative and counts **sixteen** ports. [ADR-0006](adr/0006-ports-and-adapters.md) named fifteen: it omitted `OrderIn`, which [ADR-0012](adr/0012-qr-ordering-via-cloud.md) depends on. [ADR-0021](adr/0021-corrected-port-list.md) supersedes it.

**Dependency rule, enforced by a CI test:** `pos-core` and `pos-ports` may depend only on `std`, `serde`, and pure computation crates. Adapters depend on core; core never depends on an adapter. Only the binaries know which adapters are wired in.

**Contract tests.** Each port ships a shared test suite that every implementation must pass — for example, `EventStore` must return events in order, be idempotent by ULID, and survive a simulated crash mid-transaction. This is what makes "swappable" a verified fact rather than a claim.

**Minimum state machine in core.** `pos-core` contains an explicit table of *state × event → new state*, plus invariants, for Order, Bill, Shift, and Table. Examples of invariants: a settled bill accepts no new lines; a fired line can only be voided with a reason and a permission; a closed shift accepts no transactions; payments always sum to the bill total. This table is the machine-readable twin of [pos-spec.md](pos-spec.md), and property-based tests are written against it.

## 6. Integration surface

### 6.1 Vendor groups (inbound adapters)

| Group | Examples | Notes |
|---|---|---|
| Delivery marketplaces | Grab Food, ShopeeFood | Orders arrive as events; store offline ⇒ vendor sees "busy" |
| Payment terminals | per acquirer | Unknown-result branch always exists; reconciliation resolves it |
| E-invoicing | Viettel, VNPT, MISA | Number ranges pre-allocated so invoices can be issued offline |
| ERP | SAP and others | Nightly posting of revenue and consumption |
| Shipping dispatch | Ahamove, Grab Express | Create, cancel, track; courier status becomes an event |

Adapters contain **only the vendor's protocol**. The legal lifecycle of an invoice lives in the country module; the store only knows "allocated numbers plus a queue".

### 6.2 Public API and webhooks (outbound)

Served at `api.<domain>`, versioned `/v1`, documented by generated OpenAPI.

- **Webhooks** are a **cursor over the event log**, not a separate queue. A dead endpoint means its cursor falls behind; nothing accumulates and nothing pushes back into ingestion. Delivery is at-least-once, signed with HMAC-SHA256 plus a timestamp, keyed by ULID for idempotency, retried with backoff into a dead-letter list, isolated per endpoint (bounded concurrency, circuit breaker, auto-disable after 24 hours of failure). Order is explicitly **not** guaranteed; receivers sort by ULID.
- **Cursor feed** `GET /v1/events?page_size=&page_token=` for consumers that prefer to pull and replay.
- **Writes:** menu and sold-out changes (which flow straight into the config tree and reach every store in under a second), and `POST /v1/orders` for external sales channels, which reuses the same `OrderIn` port as marketplaces.
- **SSRF protection is mandatory:** HTTPS only, private and loopback ranges blocked, resolved IP pinned, redirects not followed.

## 7. Multi-country: cells

**Latency is not the reason to split.** Because the cloud is off the sales path, a store in Tokyo served by a cloud in Vietnam performs identically to one next door: only dashboards feel the extra round trip.

**Law is the reason to split.** Data-protection regimes constrain where residents' personal data lives. So each country gets a **cell**: an independent deployment (VPS, database, broker, storage, domain) with its own country module. Cells do not know about each other. One cell failing has no effect on another, and personal data never crosses a border.

Creating a cell is not new engineering — it is the existing fork-and-deploy flow pointed at a new environment: add a GitHub Environment, add a VPS, run the workflow.

**Naming rule: redirect, never proxy.** The country label lives in the **hostname**, not the path.

```
tenant.<domain>          → home country (the first country deployed; unlabelled)
tenant.jp.<domain>       → Japan cell
api.jp.<domain>          → Japan API
```

A tiny slug→country directory (no PII) lets the home cell issue a **301 redirect** so users can type one domain. A path-based scheme (`/jp`) was rejected: it forces every Japanese request to terminate TLS in another country — destroying the legal reason for cells — adds a round trip, creates a global single point of failure, and breaks session isolation because browsers do not treat paths as security boundaries.

**Four disciplines are paid for on day one** even with a single country, because they cannot be retrofitted cheaply: internationalisation of every string; locale packs as first-class configuration (currency, timezone, date and number formats, receipt templates); host parsing that already understands a country label; and a timezone on every store.

## 8. Operations

**Health.** `/health` means the process is alive; `/ready` means database, broker, and storage are reachable and no migration is running. Deployment waits on `/ready`.

**Resilience at the edge.** All calls to the cloud use retry with exponential backoff and jitter, and **never block a sale**. Unsent events wait in the outbox. This is why the cloud needs no automatic failover.

**Backups.** Four things matter, and they are not equal:

| Data | Loss impact | Where it goes |
|---|---|---|
| Cloud PostgreSQL (+ continuous WAL archiving → RPO in minutes) | Chain-wide data loss | Garage, then replicated off-box |
| Per-store SQLite replicas | That store loses unsynced data | Garage |
| OTA artifacts | Lose fast rollback | Garage |
| **Signing keys and infrastructure config** | Lose the ability to update the fleet | **Never in the cloud** — offline USB plus sealed paper, in two locations |

A weekly job restores a random store backup *and* the cloud database to a scratch instance and verifies totals. A backup never restored is not a backup.

**Cloud can be rebuilt from the stores.** Each store retains 90 days of events, so a cloud data loss within that window is recoverable: an internal command resets a cursor and asks edges to replay from a given ULID. This is the strongest recovery property in the system and it costs almost nothing, because the data is already there.

**Resource limits are mandatory configuration**, not good intentions:

| Risk | Control |
|---|---|
| JetStream growth while a store is offline | `max_age` and `max_bytes` per stream, alert at 80% |
| Docker logs (unbounded by default) | `max-size` and `max-file` in compose |
| Old images after each deploy | prune step in the deploy job |
| Backup generations | retention policy |
| SQLite WAL growth if replication stalls | watchdog on WAL size |
| PostgreSQL growth | retention job plus partition archiving |
| In-process memory | bounded channels and caches (see AGENTS.md) |
| Metric cardinality | labels limited to store, adapter, event type, status |

**Hardware baseline.** Store server: x86-64, 4 GB RAM, SSD/NVMe (never an SD card), Windows 10+ or Linux. Cloud: 4 cores, 24 GB RAM, **local NVMe** — `fsync` is the first bottleneck in the whole system, and network block storage multiplies it by ten. Printers that open a cash drawer must be USB-attached, or all POS devices must sit on a separate VLAN, because port 9100 has no authentication.

**Menu images** are converted server-side to WebP in two sizes — a thumbnail (≤30 KB) for lists and QR menus, and a detail image (≤150 KB) — then synced to stores like any other configuration, so they display offline.

## 9. Capacity

Measured as compute and disk only; network latency excluded.

| Path | Cost |
|---|---|
| Touch → persisted → visible on other devices (in store) | **1–4 ms** |
| Event → visible on cloud dashboard | 5–25 ms compute, 60–300 ms including pipeline scheduling |
| Guest QR order → kitchen display | 0.5–2 s |
| Dashboard query (rollup) | <10 ms |
| Config change → applied in store | <1 s |

**Sizing formulas** (validated across 300–1,000 store scenarios; the shape of the tenant/brand/store tree does not matter, only totals):

Sizing formulas live in one place only — [capacity-and-reliability.md](capacity-and-reliability.md) §2 — so they cannot drift between documents. Two exceptions scale with tree shape: dashboard concurrency grows with the number of tenants, and configuration fan-out grows with the number of stores. Neither is significant at the scales considered. The only linear walls are **disk** and — once QR ordering serves guests — **bandwidth**. Full numbers in [capacity-and-reliability.md](capacity-and-reliability.md).

---

## Appendix A — Why each technology

| Technology | Why chosen | Alternatives rejected | Key number |
|---|---|---|---|
| Rust | C-level performance, memory safety, single static binary, good FFI for terminal SDKs | Go (weaker FFI, larger binaries), Node/JVM (memory, packaging) | edge <1% CPU |
| SQLite (WAL) | Embedded: nothing to install or patch per store; survives power loss | PostgreSQL per store (another service to run on every machine) | >10,000 writes/s vs 1–3/s needed |
| NATS JetStream | Outbound-only solves CGNAT; durable, light | MQTT (weaker persistence), Kafka (JVM, oversized) | 1,000 long-lived connections ≈ a few hundred MB |
| PostgreSQL | Partitioning, RLS, JSONB in one node; strong backup ecosystem | One database per store (operationally explosive) | 2,000–5,000 inserts/s vs ~222/s peak |
| Rollup tables | Dashboards always read small aggregates | ClickHouse + CDC (two more systems) | <10 ms queries |
| SolidJS | Fine-grained reactivity, no virtual DOM, small bundle — smooth on cheap tablets | React (heavier on weak devices), Leptos (younger ecosystem; decision recorded and closed) | ~7 KB runtime |
| Garage / MinIO | Self-hosted S3 at zero cost | Managed S3 (per-GB fees) | ~50–150 MB RAM (Garage) |
| Litestream | Continuous WAL shipping — the mechanism behind 5-minute machine replacement | In-house WAL shipping (kept as the fallback) | RPO seconds |
| minisign | Simple signing, self-managed keys, free | Commercial code-signing certificates | — |
| ULID | Offline generation, time-sortable, merge-safe | UUIDv4 (unsortable), auto-increment (collides across stores) | — |

## Appendix B — Rejected technologies and the admission rule

**Admission rule.** To add any infrastructure component, answer all four: (1) what does it replace? (2) what number proves it is needed? (3) how much RAM, how many processes, how many failure modes does it add? (4) can it be removed if we are wrong? Fewer than four answers means no.

| Rejected | Reason | Reconsider when |
|---|---|---|
| Valkey / Redis | All five of its jobs are already covered: caching → rollups plus page cache (queries already <10 ms); sessions → signed cookies; queues → JetStream; pub/sub → NATS and WebSocket; rate limiting → in-process counters. Its strength is read-heavy caching across many instances; this workload is *light and write-heavy* (~40 events/s peak). | Multiple cloud instances **and** PostgreSQL CPU-bound on reads |
| HAProxy / Nginx | Only one backend exists; Caddy already provides automatic TLS and will be absorbed into `pos_cloud` | Load balancing several instances (Caddy also does this) |
| Kubernetes / service mesh | Four containers on one host | Not at any foreseeable scale (GKE remains an optional lane) |
| Kafka | JetStream is sufficient and far lighter | — |
| ELK / full log shipping | Errors and metrics are shipped; full logs stay at the store and are pulled on demand | — |
| Patroni / auto-failover | The cloud being down for 30 minutes stops no sales | If the cloud ever enters the sales path |
| ClickHouse | Rollups answer in <10 ms | Large ad-hoc analytics over years of raw events |
| Replacing NATS with PostgreSQL queue + SSE | Feasible and saves one process, but requires 500–1,000 lines of hand-written ack, redelivery, deduplication and backpressure logic — exactly where "orders vanish for no reason" bugs live | Only behind the `MessageLink` port, with a fleet simulator to prove it |

## Appendix C — Dependency inventory and in-house strategy

About 25 named open-source components (200–400 transitive crates). The strategy is to **own the boundaries, not the implementations** — but four items are worth writing ourselves, ranked by benefit per unit of risk:

1. **WAL shipping** (replacing Litestream). Removes the two heaviest copyleft components (Garage/MinIO disappear entirely, because S3 only exists to satisfy Litestream), cuts 300 MB–1 GB of RAM, drops RPO below one second, and lets us compress with a shared dictionary — meaningful on metered mobile links. Blast radius is low: the outbox, not this, is what protects revenue.
2. **Dashboard** (replacing Grafana). Removes the last copyleft dependency, saves 200–400 MB, and reads rollups directly. A bug here produces an ugly chart, nothing more.
3. **Small formats** — ULID, TOTP, minisign verification, SNTP, mDNS. 50–200 lines each; removes dozens of transitive crates. **Write the format, never the cryptographic primitive:** HMAC, SHA-2, Ed25519, Argon2 stay as vetted crates.
4. **Serial port access** via native Win32/termios calls — removes the last weak-copyleft dependency and gives precise control over terminal timeouts.

**Do not write:** NATS (we use under 1% of its capacity — there is no performance to reclaim), tokio and hyper (years of scheduler tuning), SolidJS (already among the fastest), rclone (zero cost as a cron job), and above all **SQLite, PostgreSQL, TLS, and cryptographic primitives** — a category where mistakes do not show up in tests. A wrong cipher still encrypts and decrypts; only the attacker notices the timing leak.

**Counter-intuitive note:** do *not* replace VictoriaMetrics with plain PostgreSQL tables. At ~0.5–1 byte per sample versus 40–60 bytes per row, "simplifying" would burn hundreds of GB per month. Either keep it, or write a proper columnar store (delta-of-delta timestamps, XOR-compressed values, ~500 lines). At 50 stores or fewer, a third option works: sample sparsely (every 60–120 s, ~15 metrics) straight into PostgreSQL — roughly 20–40 MB/day — and skip the monitoring profile entirely.
