# Capacity and reliability

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-21

Sizing numbers, load limits, and what happens when things break.

**How to read these numbers:** they are **design estimates** on the target hardware (4 cores, 24 GB RAM, local NVMe), covering compute, disk, and pipeline scheduling. **Network latency is excluded** — it is additive and depends on where the cloud sits. The pilot measures reality and these tables get updated.

---

## 1. Store resources (per store, 200 bills/day)

| Metric | Value |
|---|---|
| `pos_edge` memory | 200–400 MB |
| CPU | under 1% average, under 5% at peak |
| Disk | 150–300 MB (90-day retention) + ~20 MB menu images + WAL |
| SQLite writes | 1–3 per second at peak (ceiling above 10,000/s) |
| LAN clients | 3–30, WebSocket fan-out under 50 ms |
| QR ordering load | **none** — guests hit the cloud; the store only receives the resulting order |

## 2. Cloud resources at three scales (QR ordering included)

| | **A** 300 stores · 200 bills · 30% QR | **B** 1,000 stores · 500 bills · 50% QR | **C** 400 small stores · 80 bills · 20% QR |
|---|---|---|---|
| Events per day | 480k | 4M | 256k |
| Peak ingest | 27/s (bursts 60–80) | 222/s (bursts 500–700) | 25/s |
| QR sessions per day | 9k | 250k | 6k |
| Concurrent QR sessions (peak) | ~150 | ~4,200 | ~100 |
| Peak HTTP requests | ~40/s | ~200/s | ~30/s |
| CPU | under 10% | 30–50% | under 5% |
| Memory | 9–10 GB | 12–16 GB | 8–9 GB |
| **Disk per day** | 290 MB → 9 GB/month | **2.4 GB → 72 GB/month** | 160 MB |
| **Bandwidth per day** | 9–15 GB | **~250 GB (7.5 TB/month)** | 6–10 GB |
| Verdict | Target VPS is heavily over-provisioned | Needs 500 GB–1 TB of disk **and** a check on the transfer allowance | Infrastructure idles |

The tenant/brand/store *shape* is irrelevant to the machine; only the totals matter. Two exceptions: dashboard concurrency scales with the number of tenants, and configuration fan-out scales with the number of stores. Neither is significant at these scales.

**Sizing formulas**

```
PostgreSQL disk  GB/month ≈ bills_per_day × 0.15 ÷ 1000
Peak ingest      events/s ≈ bills_per_day ÷ 1260        (ceiling 2,000–5,000/s)
Mobile data      MB/day per store ≈ bills_per_day × 0.003 + 2–5
Backup storage   GB ≈ stores × 0.03–0.08
```

## 3. Latency

| Path | Cost |
|---|---|
| Touch → persisted → visible on other store devices | **1–4 ms** |
| Store event → cloud dashboard | 5–25 ms compute; 60–300 ms including pipeline scheduling |
| QR order submitted → cloud processed | 5–15 ms |
| QR order → kitchen display | 0.5–2 s |
| QR menu load (server side) | 2–10 ms |
| Webhook: event → HTTP request leaves the cloud | +100–300 ms after ingest |
| API read (rollup) | under 10 ms |
| Configuration change → applied in store | under 1 s |

At peak, latency does not change — every tier stays far below the point where queues form.

## 4. Stress tests

| Test | Result |
|---|---|
| Ingest burst of 700 events/s (scenario B peak) | ~30% of the PostgreSQL write ceiling; NVMe `fsync` is the deciding factor |
| 200 stores offline for a full day, then reconnecting | 800k events drained in ~9 minutes, ~1 GB buffered — inside the configured stream limits |
| A tenant's webhook endpoint dead for 24 hours, then recovering | Its cursor falls behind; drain is rate-capped; no memory growth anywhere |
| QR adoption spiking to 80% for one hour | ~2.7 order submissions/s, ~2,400 concurrent sessions — compute is negligible, bandwidth scales linearly |
| 1,000 stores receiving a new configuration version | 1,000 messages instantly, each store applies in under a second |
| OTA ring of 500 stores downloading a 50 MB artifact | 25 GB total — must be batched, which the ring mechanism already does |

## 5. Failure and recovery matrix

| Failure | Blast radius | Detection | Fallback | Recovery | Data loss |
|---|---|---|---|---|---|
| Store server hardware dies | That store stops selling | Staff immediately; heartbeat in 30–60 s | — | Replace machine + activation code: **5–10 min** | ≤ WAL RPO (seconds); synced events already safe |
| Power loss mid-transaction | One store | On restart | UPS if fitted | Seconds (SQLite WAL recovery) | Only the uncommitted transaction |
| Store disk full | One store | Threshold alert | — | Minutes | None |
| Store network down | Store **keeps selling**; marketplaces see "busy"; **QR ordering stops** | Heartbeat | Staff take orders directly | Automatic on reconnect | None |
| **Cloud VPS down** | Dashboards, QR, webhooks, ingest stop — **every store keeps selling** | External ping / user report | Stores are autonomous | Restore in 30–60 min | ≤ backup RPO (minutes with WAL archiving) |
| PostgreSQL corruption | All cloud data | Integrity checks | Stores autonomous | Restore, **or replay from the edges** | Recoverable within the 90-day store retention |
| JetStream stream full | Sync halts; events wait in store outboxes | Queue-depth alert | — | Minutes | None |
| Bad OTA release | At most one ring | Self-test + canary | Automatic rollback, kill switch | Minutes | None — the database is copied before migration |
| Printer or display failure | One station | Print queue + red badge | Backup printer or the screen | Immediate | None |
| Payment terminal offline | Card payments handled manually | Staff | Cash or QR; bill parked for reconciliation | Immediate | None |
| E-invoicing provider down | Invoices queue | Queue depth | Issue offline against pre-allocated numbers | Provider-dependent | None |
| QR order abuse | One store's kitchen | Staff see an odd order | **Staff confirmation before firing** | Immediate | None |
| TLS certificate expiry | HTTPS and QR stop | Monitoring | — | Caddy auto-renews | None |
| Edge clock drift or tampering | Timestamps, shift boundaries | SNTP drift alert | `ClockSource` port | Immediate | Anomaly recorded in the audit log |

**The one property worth memorising:** no failure in this table stops a store from selling, except that store's own hardware — and that is a 5–10 minute swap. A cloud outage costs administration and QR ordering, never revenue.

## 6. Findings from the QR-ordering load review

| # | Finding | Action |
|---|---|---|
| 1 | **Bandwidth became a constraint for the first time.** QR ordering makes the cloud serve menu images to guests; scenario B reaches ~7.5 TB/month, above some VPS transfer allowances. | Thumbnails ≤30 KB, lazy loading, `Cache-Control: immutable` on hashed URLs; if needed, serve **images only** from a CDN — legally clean because menu images contain no personal data, unlike everything else. |
| 2 | **The cloud is now customer-visible.** Before QR, a cloud outage was invisible outside the office. | State the degradation openly: the guest page says "please ask a staff member", staff remain the primary path, and no end-customer SLA is promised for QR. |
| 3 | Cloud RPO depended on backup cadence (up to 24 hours). | Enable **continuous WAL archiving** for PostgreSQL into object storage — RPO drops to minutes. |
| 4 | **The cloud can be rebuilt from the stores**, because each edge retains 90 days of events. | Add an internal "reset cursor and replay from ULID" command. Cloud data loss inside that window becomes recoverable at almost no cost — the data is already there. |
| 5 | A printed static QR code can be photographed and used from outside the venue. | **Staff confirmation on by default**, per-table rate limits, orders accepted only during opening hours and only while the store is online. |
| 6 | Online payment for QR would pull in a large new scope. | v1 pays at the counter; payment gateways become a sixth adapter group later. |
| 7 | QR needs a second image size. | The image pipeline produces a ≤30 KB thumbnail and a ≤150 KB detail image. |

## 7. Feasibility verdict

- **Scenarios A and C** run comfortably on a single target VPS with every tier below 10% utilisation.
- **Scenario B** (1,000 busy stores) is feasible on one VPS for CPU and memory, but requires 500 GB–1 TB of disk and a check on the bandwidth allowance. These are the only two linear walls, and both are predictable from the formulas in §2.
- **QR ordering** is architecturally cheap — it reuses the existing `OrderIn` port — but it changes the system's character by placing the cloud in front of customers. That is a conscious trade-off, not a design flaw.
- **The first bottleneck under growth is disk `fsync`**, which is why local NVMe is a hard requirement for the cloud host.

## 8. The executable model (`pos-simulator`, P12)

The numbers above began as design estimates. As of P12 they are also **executable and self-checking**: `crates/pos-simulator` encodes the §2 scenarios as data and the sizing formulas as pure integer functions, so a formula that drifts from a table fails a test rather than rotting quietly. `just simulate` prints the envelope and the reconciliation report; the assertions live in the crate's tests.

What the model reproduces from the formulas, checked against the tables: **events/day** exactly (recovering the table's own implied 8 events per bill), **PostgreSQL storage** within 5% (the model gives scenario B 75 GB/month where §2 rounds to 72), **daily bandwidth** inside each published range (QR sessions × ~1 MB of menu imagery — scenario B's ~250 GB/day wall), and the §2 `÷1260` peak-ingest formula shown to be a conservative ceiling every scenario's stated peak sits under. The §4 behavioural stress tests are modelled too: the offline drain reproduces "200 stores → 800k events" and shows the ~9-minute drain is feasible within the ingest ceiling; the webhook backpressure shows a dead endpoint's cursor lag grows while its in-memory footprint stays one batch; the nightly reconciliation is the missing-id set difference; and the OTA ring rollout runs over the real `pos_core::ota::decide_rollout`.

**One standing discrepancy, filed for the pilot.** The model derives scenario A at ~18,000 QR sessions/day (bills × QR-share), where the §2 table states **9,000** — scenarios B and C both agree with the share. It is left as published and pinned by `pos-simulator`'s reconciliation report rather than silently changed, because whether A's QR share is of *bills* or of *guests*, or 9,000 is a transcription of 18,000, is a question the pilot settles.

**What is deliberately not modelled here.** The real sustained soak — 222 events/s against a live PostgreSQL with NVMe `fsync` the deciding factor, run for hours without leaking — needs the target hardware and wall-clock time, so it is an operations/pilot exercise (like the WAL-on-Windows spike and the hardware matrix), not something the deterministic model measures. `pos-simulator` is the harness that soak plugs into; the throughput figure itself is confirmed at the pilot.
