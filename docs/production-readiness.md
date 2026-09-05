# Production readiness — the checklist that owns the remaining work

**Status** Accepted · **Owner** @maintainers-architecture · **Opened** 2026-09-05 · **Scope** Vietnam pilot

This page is the single list of what stands between the tree and a shop that takes money. It exists
because the alternative failed: v1.0 was declared code-complete, and an audit then found five slices
that had shipped their domain half and never their operator half. A slice whose Rust compiles, tests
and merges *looks* finished from the inside. So the status of an item here is only ever changed by
the pull request that closes it.

## How to read this

Every item carries an **evidence state**, and that word is load-bearing:

| State | Means |
| --- | --- |
| **verified** | A maintainer read the cited source and confirmed the defect. Safe to act on. |
| **reported** | An audit lens produced it with a file:line citation. **Not yet independently confirmed.** Verify before writing code. |
| **closed** | A merged pull request closed it; the PR is named. |

The 2026-09-05 audit ran eight independent lenses over the tree and returned 38 findings. Three were
verified by hand before this page was written; the rest are **reported** and Wave 0 exists to settle
them. Treating a reported finding as fact is the same mistake this page was created to stop.

## What "production ready" cannot mean

Code closes the waves below. It does not close either of these, and no pull request should claim to:

- **Legal** ([`gate-register.md`](gate-register.md) §5) — PDPD lawful basis, consent and retention
  for customer analytics, a DPIA, the named Data Protection contact, and an independent pentest.
- **Hardware** ([`gate-register.md`](gate-register.md) §6) — the Windows Service Control Manager's
  real behaviour, headless-keyring survival across a reboot, an OTA install restarting a real box,
  power loss mid-transaction, the 222 ev/s soak, and printer/KDS/card-terminal soak on the pilot
  country's actual devices.

---

## Wave 0 — Settle the backlog *(blocks every wave below)*

| id | Item | Evidence |
| --- | --- | --- |
| V1 | Verify the 35 **reported** findings against the source; demote the ones already recorded as deliberate deferrals | — |
| V2 | Record the settled backlog here, marking each verified / refuted / already-deferred | — |

## Wave 1 — A store cannot trade

| id | Item | Evidence |
| --- | --- | --- |
| C1 | **The edge never persists the config it pulls.** `main.rs:115` boots every store on `EdgeSession::bootstrap()` — `menu: MenuCatalog::new()`, `staff: StaffRoster::new()`, `floor: FloorPlan::new()`. The pull loop rebuilds the session in memory only (`config_client.rs:20-23` says so itself), and `ConfigStore` — implemented in `store-sqlite`, contract-tested — is constructed by **no binary**. A store that restarts with its WAN down comes up with an empty menu and an empty staff roster: no line can be added and nobody can sign in. Violates [ADR-0001](adr/0001-offline-first-store-autonomy.md); triggered by the OTA installer, which restarts the edge deliberately. **Closed:** the pull loop now writes each applied document to the store's own `ConfigStore`, and `compose` restores it into the live session before anything binds — a boot-fatal read failure, because a box quietly trading on defaults while its own database holds the real menu is worse than one that will not start | **closed** |
| C2 | Every settle sets `print_receipt` and the till renders "Printing receipt…", but no binary depends on `printer-escpos` and nothing constructs a `PrintJob`. **Verified**; framed by [ADR-0100](adr/0100-receipt-and-ticket-printing.md) and sequenced as four slices: (1) the `pos-proto` `devices` node — **landed**; (2a) approval captures a device's connection and station, which a discovered proposal cannot know; (2b) the cloud compiles and publishes the node; (3) the edge applies it; (4) the dispatcher prints, with `backup_station_id` failover. Real hardware stays in the §6 gate | **verified** (1 of 4 slices landed) |
| C3 | `table_token_secret` is documented as bootstrap-generated; `bootstrap.sh` never writes it, so QR ordering and the printable table-QR sheet are off on every deployed box. **Verified and closed:** the installer now generates it on a fresh box and appends it on an upgrade run to a box that predates the line, never rewriting one it finds (a rotation would void every printed table QR); `fork-checklist.md`'s claim that the installer generated it — the reason this survived — is corrected | **closed** |
| C4 | The compiled `layout` node is authored, validated and published — and no store, edge or till reads it | reported |

## Wave 2 — Security and tenant isolation *(ADR before code)*

| id | Item | Evidence |
| --- | --- | --- |
| S1 | **`/sync` store routes trust a caller-supplied `store_id`.** `edge_config_sync` takes the tenant from the grant and the store from the path with no ownership check, so within one tenant any store's key reads a sibling store's `permissions` node — employee names and PIN hashes (**T1**). No production tenant exists yet, so this is a vulnerability, not an incident. **Closed** — and not in the two steps this row proposed: the `Grant` carried no store at all, so "an ownership check in the handler" had nothing to check against. The per-store key scoping *was* the minimal fix. `api_keys.store_id` (migration 0047) + `Grant::store()` + `require_store` on the six store-facing routes and the two relay routes; `confine_to_store` narrows the `/v1` rollup read for a store-bound key without breaking a tenant-wide integration key | **closed** |
| S2 | `StoreIdentity::for_store` stamps tenant/brand `ULID(1)` on every event and nothing replaces it, so row-level tenant isolation separates nothing | reported |
| S3 | Console security headers are layered on the base router only; the SPA document and the merged `/admin` routes ship without CSP, `X-Frame-Options` or `Referrer-Policy` | reported |
| S4 | The edge's 6-digit pairing code has no attempt limit, while the sibling PIN path has an explicit lockout | reported |
| S5 | `ReadRevenue` is carved out of `Read` because prices are T2, but the catalog placements route returns the full per-channel price book under plain `Read` | reported |
| S6 | Write the pre-production security review; gate **L5** (pentest) is sequenced after a document that does not exist | reported |

## Wave 3 — Operator surfaces that were never built

| id | Item | Evidence |
| --- | --- | --- |
| O1 | A store cannot retire a lost till: `POST /api/pair/revoke` and `GET /api/pair/devices` are mounted with no caller in `ui/` and no documented procedure. Pairings are durable by design ([ADR-0091](adr/0091-durable-edge-auth-state.md)), so nothing expires them | reported |
| O2 | Tenants, brands and devices can be created and never renamed or archived — the `PATCH` routes exist, audited and etag-guarded, with no client calling them | reported |
| O3 | The device-approval gate hides the two facts that identify a device: `name` and `address` are served and absent from the front-end type | reported |
| O4 | An expired API key renders as "Active": `expires_at_ms` is served, the dashboard type omits it, and the create form never sets one | reported |
| O5 | The store hub's "Out of stock" card counts two events nothing emits, so it is a permanent zero — and [ADR-0099](adr/0099-store-hub.md) claims the opposite. **Introduced by ADR-0099 itself** | reported |
| O6 | `EventStore::outbox_depth` is implemented in both adapters and contract-tested with no production caller, so the pending-event backlog is invisible to everyone | reported |

## Wave 4 — Close the five rails

| id | Item | Evidence |
| --- | --- | --- |
| R1 | The OTA boot report is skipped on any box not laid out for updates, contradicting `confirm_boot`'s own contract — so the fleet view holds `NULL` for exactly the boxes an upgrade campaign must find | reported |
| R2 | `Hello.product_version` is documented as reaching the fleet view; the only production `MessageLink` negotiates locally, so the release tag leaves the box on no path | reported |
| R3 | Reconciliation's edge half is deferred onto `/internal/*`, which the shipped proxy 404s for every off-box caller — the same discovery that already moved two sibling routes to `/sync` | reported |
| R4 | The edge never learns its lease standing, so a superseded box keeps updating (`lease` appears nowhere in `pos-edge` production code) | **verified** |
| R5 | `UpdateReport.tenant` is never transmitted and is filled with a nil-ULID sentinel; removing it is a `pos-ports` change and therefore ADR-first | **verified** |
| R6 | A webhook delivery has no per-attempt id, so a receiver cannot cheaply dedupe retries | reported |

## Wave 5 — Deployment and configuration

| id | Item | Evidence |
| --- | --- | --- |
| D1 | `bootstrap.sh` appends `trusted_proxy_hops` after `[artifacts]`, so it lands inside that table and is ignored — the exact failure its own comment says it prevents | reported |
| D2 | The documented ingest-cursor URL is `nats://…@nats:4222`, which cannot connect once the broker has TLS: the certificate is for `DOMAIN`, not the hostname `nats` | reported |
| D3 | `k8s/README.md` never mentions `internal_shared_secret`, which `validate()` refuses to boot without — the pod CrashLoopBackOffs on first bring-up | reported |
| D4 | A Windows store has no documented way to set `POS_EDGE_NATS_URL` or `POS_EDGE_SYNC_KEY`; the install block sets only `POS_EDGE_CONFIG` and the guide hands it a POSIX `install` command | reported |
| D5 | Windows has no generated installer (issue #182) | verified |
| D6 | `engineering-guide.md` §11.2's deploy-secrets table names a secret nothing reads, omits two the workflow requires, and contradicts `fork-checklist.md` | reported |
| D7 | `NATS_MAX_MESSAGES` / `NATS_MAX_BYTES` became a fleet ceiling in [ADR-0087](adr/0087-edge-relay-and-event-publish.md) Amendment 1 and are still hard-coded constants | verified |

## Wave 6 — Documents that contradict the tree

`README` records neither the shipped console nor the current roadmap · `O1`–`O4` name two disjoint
slice sets across `roadmap-v3.md` and `cloud-admin-ux-plan.md` · `roadmap.md`'s ADR table marks four
Accepted ADRs Open · `ui-ux.md` §5 lists five shipped console screens as backlog · the gate register
cites a gate `H13` it no longer contains · `roadmap-v3`'s A·P3 reads open for Q5/Q6/Q7 while its own
v1.0 table says they landed, and its Q7 ceiling contradicts the measurement · `fork-checklist` §1's
count is off by one · E5's entry still claims three residuals against five closed · the debate log
declares D1–D22 and lists D25 · `pos-ports`' header says eighteen ports against nineteen · the port
table credits `ConfigStore` to an adapter that does not exist · both step-budget documents say
thirteen tasks against fifteen · [ADR-0061](adr/0061-order-relay.md) is "Extended by ADR-0062", an
ADR that was never written, leaving a hole at 0062 in an otherwise dense sequence — **found while
writing this page, by the `links` gate**. *(13 items, all reported except the last, which is
verified.)*

## Wave 7 — Dead and unreachable code

| id | Item | Decision |
| --- | --- | --- |
| X1 | `shipping-ahamove`, `shipping-grabexpress` and `erp-sap` are complete, contract-tested and in no binary's dependency graph, while the roadmap records that surface as shipped | **Declare them reference adapters and correct the roadmap.** Wiring them into a binary is v1.2 (A·P5 plug-and-play), not a v1.0 correction |
| X2 | `DeviceRegistry::device_for_token` has no caller, and its docstring attributes it to a boot check that does not exist | reported |
| X3 | Neither step budget can see a tap nobody declared — needs a browser harness (gate register §8 **A4**) | verified |

## Wave 8 — The roadmap's remaining feature work

`B·W2` · `B·W3` · `B·W7` · `B·W8` · `A·P4` (printer transport, store-side WAL shipping, JetStream
capacity probe) · `A·PF` (all four) · relay live mode, which needs **ADR-0062 written first** — it
does not exist. [ADR-0061](adr/0061-order-relay.md) forward-references it as "forthcoming, in the
follow-up PR", so the accepted record points at a decision nobody made and the ADR sequence has a
hole at 0062. `roadmap-v3.md` estimates this wave at 8–10 weeks of code on its own.

**Out of scope for the Vietnam pilot:** v1.2 (international, retail) and v1.3 (Japan qualified
invoice, India IRP/UPI). Both are gated on legal registration that no pull request closes.
