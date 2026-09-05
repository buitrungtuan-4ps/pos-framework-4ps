# ADR-0068 — Fleet liveness: last-seen + config-version-held from the store pull

**Status** Accepted · **Owner** @maintainers-observability · **Last reviewed** 2026-08-27
**Relates to** [ADR-0033](0033-config-tree.md) (config tree + edge pull) · [ADR-0061](0061-order-relay.md) (relay backlog) · [ADR-0065](0065-cloud-org-registry.md) (the `stores` registry) · `docs/cloud-admin-ux-plan.md` (Track O1)

**Context.** The cloud has never recorded whether a store is actually *there*. An edge pulls its
configuration on a loop ([ADR-0033](0033-config-tree.md), WS-B) — `GET /sync/stores/{id}/config`,
carrying the config version it currently holds (`held_version`) — but the handler uses that only to
decide up-to-date-vs-deliver and then **discards it**: it never records that the store just checked
in, nor which version it holds. So the console cannot answer the first operational question a
multi-store operator asks — "which stores are online, and are they on the current config?" — and the
first observability track (O1) has no data to show. This ADR adds the smallest durable record that
answers it, captured from the signal the edge already sends.

**Decision.**

- **A dedicated `store_liveness` table**, one row per `(tenant_id, store_id)`, holding `last_seen_at`
  (Unix ms of the store's most recent contact), `config_version_held` (the version the edge reported
  holding, nullable), and `last_config_pull_at` (Unix ms of the most recent config pull). It is a
  read model for the console, kept **separate from `config_trees`** deliberately: a store is "seen"
  whether or not it has a published config tree (a freshly provisioned store pulls before anything is
  published, and the slice-2 heartbeat reports liveness with no config pull at all), so liveness must
  not be a column on a row that may not exist. It is separate from the registry `stores` row for the
  same reason the rollup read model is separate from events — a different write cadence (every pull,
  not an operator action) and a different owner (the sync path, not the admin console). The fleet read
  (a later slice) joins `store_liveness` with the registry `stores` row for the name and with the
  relay backlog for queue depth.

- **Captured on the store pull, through the `ConfigTreeStore` seam.** The capture point is
  `edge_config_sync`, which authenticates the edge's API key to a tenant and already knows the store
  and `held_version`. That handler holds exactly one persistence seam — `ConfigTreeStore` — so the
  liveness write lands there, as `record_store_seen(tenant, store, held_version, seen_at)`. This is a
  deliberate, bounded widening of that seam rather than a new generic on the core `CloudApp`: the
  config pull *is* the liveness signal, so the seam that owns config-pull persistence owns recording
  that the pull happened. The write is **best-effort**: a liveness-store failure is logged and
  swallowed, never failing the config pull the store actually needs — telemetry must not take down
  the control path.

- **RLS-isolated by tenant, exactly like `config_trees`.** `store_liveness` carries `tenant_id`,
  enables row-level security keyed on `app.tenant_id`, and grants the query role the same narrow
  access; the trusted pool-owner connection the server runs as bypasses RLS to write any tenant's row
  and to read across tenants for the console. Belt-and-suspenders, consistent with every other
  tenant-scoped table.

- **Online/offline is derived at read time, not stored.** "Online" is `now − last_seen_at ≤
  threshold`; nothing writes an offline flag (there is no event when a store goes quiet). The
  threshold is a read-side concern the fleet slice owns, so this ADR stores only the raw instant.

- **The fleet read (slice 3) is its own sub-router over a read-only join.** `GET /admin/fleet`
  (whole fleet for a `?tenant_id=`) and `GET /admin/fleet/{store_id}` (one store) are served by a
  `fleet_router` with its own `FleetState` — the same self-contained-sub-router pattern the registry
  and catalog use, so no `CloudApp` generic is added. The read is a `FleetStore` seam whose
  store-postgres impl `LEFT JOIN`s the registry `stores` row (identity, status), `store_liveness`
  (last-seen, held version, last pull), the config tree (published version = the id of the last
  element of `ConfigTreeState.history`, extracted in SQL), and an aggregate over `order_queue`
  (pending count + oldest-pending instant) — one row per store, so the console never pulls a whole
  tree or queue to summarise. Both routes are gated by the existing `console.data.read` permission
  (every console role, so Ops and Viewer see the fleet); a store un-configured, never-seen, or with an
  empty queue simply carries `null`/`0` in those fields. The handler derives `online` (against a
  180-second freshness window, a few pull/heartbeat cycles of slack) and `config_current` (held equals
  published) at read time, so the answer needs no background sweep. `/admin` is absent from the gated
  OpenAPI (like `/internal` and `/sync`), so the fleet routes add no drift-gate surface.

- **Background-task health (slice 4) is a heartbeat, not a backlog scrape.** The cloud's off-request
  loops — the rollup projector, the retention/PII sweep, the webhook dispatcher — each upsert one row
  into a `task_health` table at the end of every tick, carrying the instant and a small
  self-describing `detail` (`{"ok":…,"interval_secs":…,…}`). `GET /admin/health/tasks` (its own
  sub-router, behind `console.data.read`) reports each *expected* loop plus any extra that ticked, and
  derives `healthy` at read time from `now − last_tick_at` against the interval the tick itself
  recorded (times a slack of a few cycles) **and** the tick's `ok` — so a loop that has gone quiet, a
  loop dead since boot (no row → reported unhealthy, not hidden), and a loop that is alive but whose
  work is failing are all distinguishable. This mirrors the liveness read: the producer writes the
  raw facts, the reader derives the verdict, and `task_health` is fleet-wide server state — no
  `tenant_id`, no RLS — because these loops run once per cloud, not per tenant. A recording failure is
  swallowed (a loop must never crash because its telemetry write failed). Rejected for this slice:
  scraping each loop's *backlog* (rollup-cursor lag, order-queue depth) to infer health — a growing
  backlog is a useful signal but does not distinguish "caught up" from "stopped," which the heartbeat
  does directly; per-loop backlog gauges can layer on later.

- **The Fleet dashboard (slice 5) is one read-only operational screen.** A new console screen
  (`/fleet`, tenant-scoped in the nav) reads both O1 endpoints and shows them in one glance: a
  system-health strip across the top (the background loops from slice 4, each with a healthy/unhealthy
  badge and its last-tick age) above a table of the tenant's stores (online/offline, last seen,
  config in-sync/behind, relay backlog with oldest-pending age), with a per-store detail drawer. It
  polls on a fixed interval so "online" and "last seen" stay current without a manual refresh, reuses
  the F2 kit (`DataTable`/`StatusBadge`/`Drawer`/`TechnicalDetails`) and the F0 context gate, and adds
  no new client capability — the verdicts (`online`, `config_current`, per-loop `healthy`) are already
  computed server-side, so the screen only presents them. Relative times are formatted with
  `Intl.RelativeTimeFormat`, so no per-unit catalogue string is added; the ULID stays behind a
  Technical-details disclosure as elsewhere.

**Rejected.**

- **Columns on `config_trees` or on the registry `stores` row** — rejected: liveness must exist for a
  store with no published config and (slice 2) with no config pull at all, and its per-pull write
  cadence does not belong on an operator-administered registry row.
- **A new `FleetStore` generic on `CloudApp`** threaded through every handler — rejected for slice 1:
  a single new capability does not justify adding an eighth type parameter to ~50 handler signatures.
  The read side introduces its own sub-router with its own state (the established pattern), and the
  write reuses the one seam the pull path already carries.
- **Liveness as an event on the log** — rejected: a per-pull event on every store is a high-volume
  stream to answer a point-in-time question a single upserted row answers; the rollup read model made
  the same call.

**Consequences.**

- One additive migration (`0020_store_liveness`), forward-only, RLS-isolated. No change to
  `PROTOCOL_VERSION` (the edge already sends `held_version`), no permission-identifier change.
- `ConfigTreeStore` gains one method (`record_store_seen`), with a store-postgres impl and the
  in-memory fake updated in lockstep, exercised by the real-PostgreSQL integration suite.
- Landed in slices under Track O1: (1) capture (this) → (2) edge heartbeat → (3) fleet read API →
  (4) background-task health → (5) fleet dashboard. Alerting on the derived state is Track **O2**, not
  here.

## Amendment 1 — the heartbeat carries the store's own publish backlog (Track O6)

Slice 2 shipped the heartbeat as a bare "I am here": no body, no facts. That left the fleet row able
to say how many orders the cloud was holding **for** a store and nothing at all about how many sales
the store was holding **from** the cloud — a box whose event link had been down for a day read
identically to one perfectly current. `EventStore::outbox_depth` existed for exactly that question,
was implemented in both adapters and contract-tested, and had no production caller anywhere.

The heartbeat is where the answer belongs: it is the one rail that reaches the cloud on a fixed
interval whether or not the store has anything else to say, and a depth is a count — never an event
body — so it carries no personal data (`docs/pos-spec.md` §13). So:

- `HeartbeatTransport::beat` takes a `HeartbeatReport`, and the loop reads it from a
  `HeartbeatSource` — the shipped one being the store's own `Edge`. A log that cannot be read reports
  `None` and still pings: a box that stopped saying "I am here" because it could not count its outbox
  would read as offline, which is a worse lie than an unknown depth.
- `POST /sync/stores/{id}/heartbeat` accepts an **optional** JSON body. An edge older than the body
  posts nothing and is recorded exactly as before; a body that is present and will not parse is
  refused, because swallowing it would let a store report nothing forever while believing it
  reported.
- Migration `0049` adds nullable `outbox_depth` and `outbox_reported_at` to `store_liveness`, and the
  upsert `COALESCE`s both: silence never overwrites the last depth a store did report. **`NULL` is
  not zero** — "did not say" and "nothing pending" are different answers, and a console that rendered
  both as `0` would report a silent store as caught up. The console shows *Not reported* for the
  first and a number for the second, and the reporting instant travels with the depth so a stale one
  reads as stale.

No `PROTOCOL_VERSION` change: the body is additive and optional in both directions.
