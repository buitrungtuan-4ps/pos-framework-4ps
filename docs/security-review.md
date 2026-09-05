# Pre-production security review

**Status** Accepted · **Owner** @maintainers-security · **Last reviewed** 2026-09-05
**Classification** T3 Internal — the fork-neutral posture, not a store's data

The review production-readiness **S6** asked for, and the document gate **L5** (an independent
pentest) is sequenced after. It states what the system's security posture *is*, surface by surface,
with the file that implements each claim; then it lists what is still open, and what a pentest should
actually spend its time on.

Scope is the **Vietnam pilot**: the cloud tier as `deploy/compose.yml` runs it, the store server
(`pos_edge`), the two browser front-ends, and the rails between them. JP and IN are out of scope —
their go-live is gated on registrations that do not exist yet (gate register **X1**/**X2**).

Two earlier notes stay valid and are not repeated here: [`security-review-ws-d.md`](security-review-ws-d.md)
(the ops-hardening pass) and the per-phase reviews recorded in the ADRs it cites.

---

## 1. The tiers, and what an attacker reaches from where

Three trust zones, deliberately not four:

| Zone | What runs there | Reachable from |
| --- | --- | --- |
| **Cloud** | `pos_cloud`, PostgreSQL, NATS JetStream, Garage, the console SPA | The public internet, through a reverse proxy that terminates TLS ([ADR-0044](adr/0044-fork-and-deploy.md), [ADR-0090](adr/0090-tls-postures.md)) |
| **Store LAN** | `pos_edge`, its SQLite database, the till/KDS browsers, printers | Anyone on the shop's network |
| **Device** | A paired browser holding a device token | The person holding the tablet |

The store never accepts an inbound connection from the cloud. Every cloud→store flow is a store-initiated
pull: config (`GET /sync/stores/{id}/config`), the order relay's long-poll, the heartbeat, the OTA
fetch. That is an architectural property ([ADR-0001](adr/0001-offline-first-store-autonomy.md),
[ADR-0061](adr/0061-order-relay.md)), and it is the reason a shop needs no inbound firewall rule and
no port forward: **there is no cloud-side capability to reach into a store**, so a compromised cloud
cannot address a till directly. It can still publish a config or an OTA plan the store will pull —
see §6.

## 2. Authenticated surfaces

Every route in the tree sits behind exactly one of these gates. There is no unauthenticated write
anywhere except the two bootstrap exchanges, which are single-use by construction.

### Cloud

| Surface | Gate | Where |
| --- | --- | --- |
| `/v1/*` (integrator API) | Bearer API key → tenant + scopes | `auth/apikey.rs`, `auth/bearer.rs` |
| `/sync/*` (store rails) | The same bearer, additionally **bound to one store** | `require_store`, production-readiness **S1** |
| `/admin/*` (console) | Session cookie → admin identity → **per-route permission** | `auth/session.rs`, `auth/console_rbac.rs` — 180 `require_permission` call sites |
| `/internal/*` (ingest) | A shared secret in a header, ≥32 chars, refused at boot if short | [ADR-0097](adr/0097-internal-route-authentication.md), `config.rs` |
| `/health` | None, by design — a liveness probe with no data in it | `health.rs` |
| `POST /admin/login`, `/admin/setup` | Credential exchange (see below) | `auth/admin.rs` |

### Store server

| Surface | Gate | Where |
| --- | --- | --- |
| `/api/*` domain routes | Paired **device token**, then a **signed-in employee** | `http/auth.rs` (`require_paired_device` → `require_signed_in`) |
| `/ws` | The same device token, over `Sec-WebSocket-Protocol` because a browser cannot set a header on an upgrade | `http/auth.rs` |
| `/api/pair/devices`, `/api/pair/revoke` | Paired device token | `http/pair.rs` |
| `POST /api/pair` | The six-digit pairing code — single-use, five-minute TTL, budgeted (see §3) | `pairing.rs` |
| `POST /api/activate` | The one-time activation code from the setup sheet | [ADR-0050](adr/0050-activation-code-exchange.md) |
| `/healthz` | None | `http/health.rs` |

The two unauthenticated routes are the bootstrap exchanges, and both spend their credential: an
activation code pairs one box and is then dead; a pairing code pairs one device and is removed on
redemption.

## 3. Credentials — what exists, how it is stored, what throttles it

| Credential | Held as | Throttle |
| --- | --- | --- |
| Super-admin password | **Argon2id** with a per-user salt | Sliding-window limiter *before* the verify runs, so guessing costs the attacker a `429`, not the server a hashing storm (`auth/rate_limit.rs`) |
| Super-admin second factor | **Mandatory TOTP**, plus one-time recovery codes | Same limiter; a code is single-use |
| Console session | A 256-bit CSPRNG token; only its **SHA-256** is stored | Sliding idle TTL, listable and revocable by the owner |
| API key | A CSPRNG secret; only its **SHA-256** is stored, shown once | Per-tenant limiter on `/v1/orders`, per-connection on `/sync` |
| Staff PIN | **Argon2id**, 4–8 digits | Per-device attempt lockout at the edge — the PIN's defence is the cost plus the lockout, never the digit count |
| Device token (till) | A 128-bit CSPRNG value; only its **SHA-256** reaches disk or the process map ([ADR-0091](adr/0091-durable-edge-auth-state.md)) | Retirable per device (**O1**) |
| Pairing code | Six digits, five-minute TTL, in memory only | 10 consecutive failures shut the endpoint for 60s, checked *before* the code table is read (**S4**) |
| Store sync key | OS keyring, or a mode-0600 env file — **never** `config.toml` | Scoped to one store (**S1**) |
| Artefact signing key | **Never on a runner or a VPS** — offline custody (gate **H1**/**H3**) | n/a |

A plain SHA-256 is the right primitive for the machine-generated credentials and the wrong one for the
human-chosen ones, and the split above is deliberate: a 128- or 256-bit CSPRNG value has no dictionary
to run and no salt to add, and its lookup sits on the gate every request crosses. A password and a
4-digit PIN both get Argon2id.

**A digest is not a credential here.** The gate hashes what the client presented and compares digests,
so holding a stored digest authenticates nothing — a stolen `pos.db` or a dumped `api_keys` table
yields values that cannot be presented.

## 4. Tenant isolation

Three independent layers, and the cloud does not rely on any one of them:

1. **Row-level security** on 40 tables (`ENABLE ROW LEVEL SECURITY` + a `tenant_id = current_setting('app.tenant_id')` policy in every migration that creates one).
2. **An explicit `WHERE tenant_id = $1`** in every adapter query. The server connects as the trusted pool owner, which *bypasses* RLS — so the filter is the primary control and RLS is the second line, not the reverse. A reader who assumes the RLS policy is what isolates tenants will misread this code.
3. **The cloud stamps the tenant on ingest** ([ADR-0101](adr/0101-the-cloud-stamps-the-tenant.md)): an event's tenant is looked up from the store registry, not believed from the envelope the store sent, so a mistyped or forged claim cannot mis-file another tenant's history.

The console is deliberately **global** (a super-admin sees every tenant) and names its tenant in a
`?tenant_id=` query ([ADR-0060](adr/0060-cloud-back-office-dashboard.md)); isolation there is the
RBAC permission, not the tenant.

## 5. Data classification, and where each tier lives

Following the organisation's T1/T2/T3 scheme:

- **T1 Restricted** — staff PIN hashes, employee records, and any customer identifier a subject
  request touches. PIN hashes live in `employees` and ride to the store on `/sync`; they are
  **stripped unconditionally** from both console config reads (**S7**), because no console screen
  needs one. Subject-request tooling exists ([ADR-0076](adr/0076-subject-request-tooling.md)) and hands the
  payload to a person — it never fulfils a rights request autonomously.
- **T2 Confidential** — prices, the compiled menu, tax rates, vendor terms. `ReadRevenue` is carved
  out of `Read` for exactly this (**S5**), so Ops and Viewer see the menu structure and not what
  anything costs.
- **T3 Internal** — fleet liveness, task health, audit metadata, this document.

**Logs and metrics carry no personal data by construction.** The request-tracing layer records
method, path and status and never a body; the metrics label alphabet forbids names, emails, phones
and identifiers, with a compile-time assertion that a label can never hold a ULID.

## 6. Supply chain and the artefact trust chain

- **The store verifies before it installs.** An OTA artefact is minisign-verified against a key list
  compiled into the binary; an unsigned or wrongly-signed artefact is refused, self-test failure
  rolls back ([ADR-0047](adr/0047-minisign-verification.md), [ADR-0092](adr/0092-artifact-trust-chain.md)).
  This is the control that limits what a compromised cloud can do to a store: it can publish a plan,
  and the store will still refuse a binary it cannot verify.
- **The signing key never touches CI or the VPS** (gates **H1**–**H3**). `release.yml` fails before
  it builds if the public half is unset.
- **Dependencies**: `cargo deny check advisories` on every PR and nightly; `deny.toml` also fixes the
  licence and source allow-lists.
- **Secrets**: a `gitleaks` job on every PR, over full history, installed from a pinned checksum
  rather than a third-party action.
- **Actions**: every GitHub Action is pinned to a commit SHA, enforced by `xtask actions-pinned`.

## 7. Inbound untrusted data

- **Guest QR ordering** is the only path where an unauthenticated stranger reaches the system. The
  token is HMAC-SHA256, verified in constant time, and binds `store_id` + `table_id`, so it cannot be
  forged without the server secret nor replayed on another table. Abuse is bounded by guardrails — per-table
  rate limit, business-hours gate, online-only gate, staff confirmation on by default — because the
  token is a *printed* credential and cannot expire on its own. The endpoint is off unless a secret is
  configured.
- **Webhooks out** are HMAC-signed (`webhook/sign.rs`) and pass an **SSRF filter** (`webhook/ssrf.rs`)
  before dispatch, with a circuit breaker that disables a dead endpoint rather than hammering it.
- **Relayed orders and pulled config** fail closed at the edge: a malformed order is acked
  `invalid_argument` rather than dropped or panicked; a malformed config node leaves the running
  session unchanged rather than blanking a trading store's menu.
- **Every ULID field parse in the cloud goes through one helper** (177 sites), so a malformed id is a
  named `400`, never a panic and never a silent default.

## 8. Open items

Ranked by what a real attacker would reach first. None is a live exposure today — **no production
tenant exists** — but each is a decision or a task before the first store trades.

| # | Item | Severity | Status |
| --- | --- | --- | --- |
| **S8** | The console config read returns the compiled `menu` node, so a Viewer can read prices (T2) through the config screen even now that the placements route is closed. Two answers: gate the whole read on `ReadRevenue` (Ops loses the config screen), or redact the node for a caller without it | Medium — a role boundary that does not hold | **Owner's call**, recommended: redact the node |
| **P2** | On a headless Linux box the kernel keyring is not durable across a reboot, so the store's sync key may need re-supplying. The production answer is a TPM-sealed credential | Medium — availability, not disclosure | Tracked hardware gate |
| **H12** | Publishing NATS `4222` makes the broker reachable wherever the store addresses are knowable. Restrict at the host firewall | Medium | Human gate, documented |
| **#312** | The JetStream caps are a fleet ceiling, not a per-store one, so one noisy store can consume the fleet's headroom | Low — availability | Flagged |
| **#307** | A webhook delivery has no per-attempt id, so a receiver cannot cheaply dedupe retries | Low | Flagged |
| **#303** | The edge never learns its lease standing, so a superseded box still updates | Low | Flagged |
| **A4/#308** | The click-budget check cannot see an undeclared tap — it needs a browser e2e harness. Not a security control, but it is the reason no automated test drives a real browser today | Low | Flagged |

## 9. What the pentest (gate L5) should target

A generic web-app sweep will find little here and waste the engagement. The five places worth the
time, in order:

1. **The `/sync` store binding.** `require_store` is what stops one shop's key reading another's
   config, orders and staff PIN hashes. It was a real gap once (**S1**). Try every route under
   `/sync` with a key scoped to a different store, and with a store id that does not exist.
2. **Console RBAC.** 180 permission checks, hand-written per route. The interesting question is not
   whether they are present but whether any is the *wrong* one — the `ReadRevenue` carve-out (**S5**)
   and the PIN-hash leak (**S7**) were both exactly that. Enumerate `/admin` as a Viewer and as an Ops
   role and diff against what each should see.
3. **The pairing and activation exchanges.** Both are unauthenticated by necessity. Attack the
   budget (**S4**), the single-use property, and the five-minute TTL; try to pair a second device on
   one code, and to redeem a spent activation code.
4. **Guest QR.** Forge a token, replay one table's token on another, and try to walk the guardrails
   (rate limit, hours, online-only) rather than the signature.
5. **The `/internal` secret** ([ADR-0097](adr/0097-internal-route-authentication.md)) and the proxy denial in
   front of it: confirm the route is unreachable from outside *and* refuses a wrong secret from
   inside. Depth, not either alone.

Out of scope for the engagement, because it is a hardware property rather than a software one:
the TPM-sealed keyring (**P2**) and the physical custody of the signing key (**H1**–**H3**).

## 10. Regulatory posture

Not a legal opinion — the gates that need a person, restated so the pentest report can reference them:

- **Vietnam PDPD (Decree 13/2023)** — the lawful basis for processing must be confirmed and recorded
  before the first live store (**L1**); consent status, retention period and a DPIA are required for
  customer analytics (**L2**); a cross-border transfer needs a DTA or explicit consent (**L3**).
  `retention_days` is a value a person chooses, and the system enforces whatever it is told.
- **GDPR** — the tooling applies data minimisation and hands an EU-resident rights request to a named
  Data Protection contact (**L4**); it never fulfils one autonomously.
- **CCPA** — no flow to third-party ad tech or a data broker exists in this tree. If one is added, it
  may meet the "sale" definition and needs surfacing before it ships.
