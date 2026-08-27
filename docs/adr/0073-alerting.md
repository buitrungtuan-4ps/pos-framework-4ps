# ADR-0073 — Alerting: server-side detection, storage, and delivery of operational conditions

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-27
**Relates to** [ADR-0068](0068-fleet-liveness.md) (fleet liveness + task health this reads) · [ADR-0032](0032-webhooks.md) (the webhook transport a channel reuses) · [ADR-0069](0069-audit-trail.md) (the nullable-tenant, RLS-isolated table shape this mirrors) · [ADR-0053](0053-cloud-sync-port.md) (`MessageLink::capacity`) · `docs/cloud-admin-ux-plan.md` (Track O2)

**Context.** Track O1 gave the console a *read model* — a Fleet screen and a task-health endpoint — but
nothing **watches** it. An operator learns a store has been offline for an hour only by looking; a
webhook endpoint auto-disables silently; the rollup projector can fall behind or start failing and no
one is told; the store→cloud JetStream can approach its limit with no warning. The signals exist and
are computed:

1. **Fleet liveness** (`FleetRow.last_seen_at`, `relay_backlog`, `relay_oldest_pending_at`) is captured
   on every config pull/heartbeat but only ever read on demand by the Fleet screen.
2. **Task health** (`task_health`, ADR-0068 slice 4) records each background loop's last tick and an
   `ok`/`failed` detail, but nothing acts on an unhealthy tick.
3. **JetStream capacity** — `MessageLink::capacity()` returns a `LinkCapacity` with an `is_at_least(pct)`
   helper (ADR-0053), used nowhere.
4. **Webhook auto-disable** — the dispatcher flips an endpoint's `disabled` flag after a day of
   continuous failure (ADR-0032), silently.

And some signals are computed but *never leave the edge or don't exist yet*: SNTP clock drift
(`Drift::Alarm` in `pos-edge` is computed-but-unread, with no producer and no path to the cloud),
free-disk (no reading anywhere), a print-failure event (printer failure is an edge port error, never a
domain event, so it never reaches the cloud event log), and fiscal invoice-range exhaustion (fiscal has
not landed).

**Decision.** Add a **server-side alert engine**: a periodic evaluator that reads the existing cloud
read models, decides which conditions are firing, and reconciles them against a durable `alerts` table
with an open→resolved lifecycle; a delivery step that pushes newly-opened alerts to configured channels;
and a console surface (a fleet-wide alert list the notification bell and an Alerts screen read). No new
signal is invented where one already exists — the engine is the missing *watcher*, not new telemetry.

- **A pure evaluator (`pos_cloud::alerts::evaluate`).** Given a snapshot — `now`, per-tenant fleet rows,
  each tenant's disabled webhook endpoints, the task-health rows, an optional JetStream `LinkCapacity`,
  and a `AlertThresholds` — it returns the set of `FiringAlert`s. Pure and fully unit-tested; it does no
  I/O, so the loop that gathers the snapshot and the store that persists the result are testable in
  isolation. This mirrors the `floor_compiler`/`catalog_compiler` split: domain decision as a pure
  function, I/O around it.
- **An `alerts` table with an open→resolved lifecycle (`AlertStore`).** One row per *active* condition,
  identified by `(tenant_id, kind, dedup_key)` — the store id for a store-scoped alert, the endpoint id
  for a webhook one, empty for a server-wide one. Re-evaluation *refreshes* an already-open alert's
  `last_seen_at` rather than opening a duplicate (a partial unique index enforces one open row per key);
  a condition that stops firing is **resolved** (its `resolved_at` set), so a store coming back online
  clears its own offline alert without an operator touching it. The table mirrors `audit_log`:
  nullable `tenant_id` (server-wide alerts carry none), RLS-isolated, written and read fleet-wide by the
  trusted pool connection. Nothing sensitive is stored — an alert is a condition and a small JSON
  detail (counts, ages, a version string), never a payload or PII.
- **The in-console channel ships; off-console push is a flagged follow-up.** The **in-console channel
  is the table itself**: the evaluator persists every firing alert, and the notification bell and the
  Alerts screen read them through `GET /admin/alerts`. That is the primary, immediately-useful delivery
  path and it ships in this track (the loop, the read API, and the screen). The **off-console push
  channels are a separable follow-up**: a webhook channel reusing the ADR-0032 `WebhookTransport`
  (SSRF-safe, TLS, HMAC-signed — an alert is just another signed JSON body to a vetted URL), and email
  and a Zalo/Telegram chat channel as further adapters. They plug into an `AlertChannel` seam that
  delivers the newly-opened set the reconcile already returns, and none of them re-architect the
  engine; keeping them out of the first cut lets the detection-and-surfacing path land complete and
  reviewed before the push transports are wired.
- **The evaluator runs as one more background loop**, recording its own `task_health` row exactly like
  the projector/retention/dispatcher loops (so the watcher is itself watched). Its cadence and every
  threshold (offline seconds, backlog limits, JetStream percent) are `CloudConfig` tunables with
  sensible defaults.

**Conditions this ADR ships** (all have a ready data source today):

| Alert | Source | Scope |
|---|---|---|
| Store offline | `FleetRow.last_seen_at` older than the threshold (its own, ≥ the Fleet screen's 3-min derived view — O2 wants 5 min) | per store |
| Relay backlog stuck | `relay_backlog` over a limit, or `relay_oldest_pending_at` older than a threshold | per store |
| Webhook endpoint disabled | the tenant's endpoints with `disabled = true` (auto-resolves when re-enabled) | per endpoint |
| Projector unhealthy | the projector's `task_health` row: stale (no tick within interval+slack), `ok=false`, or `failed>0` | server-wide |

**Conditions deferred (flagged), each needing upstream telemetry that does not exist yet** — recorded
here so the gap is explicit, not silently dropped:

- **Clock drift.** `Drift::Alarm` is computed-but-unread and has *no producer* (no SNTP poll runs in
  CI/today) and no path to the cloud. Wiring it needs an edge SNTP poll → a drift field on the
  heartbeat/an event → the cloud. A `ClockDrift` alert slots into the evaluator the day that field
  arrives.
- **Disk low.** No free-disk reading exists anywhere; `metrics-vm` is a write-only sink. Needs an
  edge-reported disk figure (a heartbeat field) before an alert can read it.
- **Print-error spike.** Printer failure is an edge `PortError`, never a domain event, so it never
  reaches the cloud event log, and `EventStore::read` has no by-type filter. Needs a new `pos-proto`
  event, edge emission, and a by-type query first.
- **E-invoice backlog / invoice-range exhaustion.** Fiscalization has not landed; there is no backlog
  or range to read.
- **Projector *failure streaks*.** `task_health` keeps only the latest tick, not a history, so this ADR
  alerts on the latest tick being unhealthy; counting consecutive failures needs a small history table
  and is a follow-up.
- **JetStream near capacity (the cloud-side probe).** The evaluator *supports* this condition — it
  takes a `LinkCapacity` and applies `is_at_least(pct)` — but nothing feeds it a reading yet: the
  cloud consumes the stream through `NatsConsumer`, which exposes no capacity read, and NATS ingest is
  optional. A cloud-side stream-info probe is a small, self-contained follow-up; the evaluator fires
  the alert the day one is wired.

**Consequences.**

- **Positive.** The console stops being blind between glances: the conditions that already have signals
  are watched and surfaced, and delivered off-console over the existing webhook transport. The pure
  evaluator makes the alert logic exhaustively testable without a database or a clock. The open→resolved
  lifecycle means alerts self-heal, so the list reflects reality rather than accreting stale entries.
- **Negative / cost.** The evaluator polls (it is a loop, not push), so an alert fires within one
  cadence of the condition, not instantly — acceptable for operational alerts (minutes, not seconds).
  The deferred conditions mean the "six-item minimum set" is not complete on day one; the table above
  makes the partial coverage explicit and each gap is a scoped follow-up rather than a re-design.
- **Neutral.** Alert thresholds are cloud-wide config, not per-tenant — a per-tenant override is a later
  refinement if a tenant needs a different offline tolerance.
