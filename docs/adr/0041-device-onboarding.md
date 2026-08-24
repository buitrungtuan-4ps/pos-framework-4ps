# ADR-0041 — Device onboarding is discover → propose → admin-approves, over a proposal table

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-08-20
**Relates to** [ADR-0026](0026-port-shapes.md) · [ADR-0033](0033-config-tree.md) · [ADR-0037](0037-api-keys.md) · [ADR-0039](0039-config-delivery.md) · `docs/roadmap.md` P7

**Context.** `docs/roadmap.md` P7 requires a **printer/KDS discover → propose → admin-approves** flow.
A store finds a printer or kitchen-display panel on its LAN and wants it usable, but a device must not
become live on a tenant's fleet just because it answered an mDNS query — port 9100 has no
authentication at all, so an unapproved printer is an open door. The gate is a human: the store
*proposes* a discovered device, and a super-admin *approves* it before it is anything more than a
pending suggestion.

Two questions this settles: how a proposal reaches the cloud (there is no `device.discovered` event in
the catalogue — device discovery is operational, not part of the sales event stream), and how approval
is expressed.

**Decision.**

- **A proposal is a store-initiated write on the store-facing surface.** The store POSTs a discovered
  device to `/sync/stores/{store_id}/devices` — the same store-facing surface config delivery uses
  ([ADR-0039](0039-config-delivery.md)) — authenticated by an API key with a new deny-by-default
  `manage_devices` scope, answering only for the key's own tenant. The body carries the discovered
  facts: `kind` (`printer` or `kds`), a human `name`, and an `address`. The cloud stores it **pending**
  and mints its id. Discovery stays entirely on the store's LAN; only the proposal crosses to the
  cloud, so the cloud never probes a store's network.

- **Approval is a super-admin action that flips a status, and it is the gate.** A super-admin lists a
  tenant's pending proposals (`GET /admin/devices/proposals?tenant_id=…`, behind the session guard,
  [ADR-0034](0034-super-admin-auth.md)) and resolves each — `POST …/{id}/approve` or `…/{id}/reject`.
  Approval sets the row to `approved`; rejection to `rejected`. A device is usable only once approved,
  and the store reads its **approved** devices back from `GET /sync/stores/{store_id}/devices` — so the
  edge acts on the admin-approved set, never on a raw discovery. Resolving is idempotent on a
  already-resolved row (only a `pending` row transitions), and tenant-scoped so one tenant cannot
  resolve another's proposal.

- **A proposal is its own small table, not a config-tree edit (yet).** The `device_proposals` table
  holds the durable facts and the status, RLS-isolated by tenant exactly as the other cloud tables.
  Keeping proposals separate from the config tree keeps a pending or rejected device out of the
  validated, versioned config a store applies — a device becomes *config* only when an operator later
  authors it, which is a modeling step ([ADR-0033](0033-config-tree.md)) this does not force. The
  approved-device list is the contract the edge reads in the meantime.

- **The routes are a merged sub-router, not new `CloudApp` collaborators.** Device onboarding needs the
  proposal store plus the existing admin, api-key, and clock collaborators, across four routes. Rather
  than thread an eighth generic through every unrelated handler, it is a self-contained sub-router with
  its own bundled state, merged into the main router in `main` — the same shape reconciliation took
  ([ADR-0040](0040-reconciliation.md)). `CloudApp` stays at its seven collaborators.

**Rejected.**

- **Auto-adding an approved device to the config tree** — rejected for this slice: it couples approval
  to a config publish and a `devices` schema the config tree does not yet model, and risks pushing an
  incoherent version. Approval records approval; the approved-device list is queryable; authoring the
  device into config is a later, separate step.
- **A `device.discovered` event on the ingest stream** — rejected: there is no such event in the
  catalogue, and discovery is out-of-band operational state, not a sale. Reusing ingest would put
  LAN-scan noise into the durable event log.
- **Letting a store self-approve** (no human gate) — rejected: it defeats the entire point. An
  unauthenticated port-9100 printer that anyone on the LAN can add is exactly what the admin gate
  exists to stop.
- **An eighth `CloudApp` generic** — rejected: the collaborator count is already high; a merged
  sub-router keeps device concerns in one place and `CloudApp` from growing without bound.

**Consequences.**

- `store-postgres` migration `0007` adds `device_proposals` (id, tenant_id, store_id, kind, name,
  address, status, timestamps), RLS-isolated by tenant, with a `tenant_id` index and a partial index
  on the pending queue. A `DeviceProposalStore` seam is filled by a `PostgresDeviceProposals` query
  through the `persistence.rs` bridge, and by a fake in the route tests.
- One new `manage_devices` API-key scope joins the deny-by-default set; a store key holds it to propose
  and to read its approved devices.
- The endpoint is the cloud half. The `pos_edge` discovery loop (mDNS scan → propose) and the edge's
  use of the approved-device list are store-side wiring (P5/P9); the contract between them is these
  routes' shapes.
- Proven over the fakes end to end: propose → appears pending → admin approves → appears in the store's
  approved list → a second resolve is a no-op; and the SQL is exercised in `store-postgres`'s gated
  integration suite.
