# ADR-0068 — Fleet liveness: last-seen + config-version-held from the store pull

**Status** Accepted · **Owner** @maintainers-observability · **Last reviewed** 2026-08-27
**Relates to** [ADR-0033](0033-config-tree.md) (config tree + edge pull) · [ADR-0058](0058-outbound-pull-relay.md) (relay backlog) · [ADR-0065](0065-cloud-org-registry.md) (the `stores` registry) · `docs/cloud-admin-ux-plan.md` (Track O1)

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
