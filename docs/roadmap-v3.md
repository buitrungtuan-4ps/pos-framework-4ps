# Roadmap v3 — Production-ready, international, plug-and-play

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-02
**Supersedes the planning horizon of** `docs/cloud-admin-ux-plan.md` (roadmap v2, delivered through PR #70)
**Companion** [ADR-0083](adr/0083-integration-doctrine.md) (the integration doctrine this roadmap's plug-and-play principle rests on)

Roadmap v2 built the framework's skeleton: a pure `pos-core`, an event-sourced edge,
a config-tree cloud, and 38 admin screens. This roadmap takes it from *skeleton* to
*production-ready international POS core*: it fixes what the domain audit found missing,
generalises everything that risked becoming a per-country special case, and holds every
external system at arm's length behind a plug-and-play boundary.

Two audits stand behind it — a domain audit (order flow, fraud/RBAC, receipt, tax) and a
platform audit (config tree, floor plan, the whole HTTP surface, and both UIs) — plus
targeted checks on the kitchen-print chain, tender/terminal generalisation, the realtime
contract, and non-functional gates. Every slice below traces to a finding with file/line
evidence; this document is the durable form of that plan.

## Principles (unchanged, load-bearing)

1. **Dễ sử dụng** — easy to use. A normal operator sells without training; a step budget guards it (Q7).
2. **Dễ quản lí** — easy to manage. One cloud is the source of truth; drift is visible to the device.
3. **Dễ cập nhật** — easy to update. Ship and roll back a fleet from one dashboard button.
4. **Optimize & performance** — low latency, low resource, high CCU, proven by CI gates (A·PF).
5. **International standard, every format** — FnB → retail → drink shop by config, not by a rebuild.
6. **Core stays small — everything else is plug-and-play** — see ADR-0083.

## Two programs, run in parallel

- **Program A — Ship the Edge** (infrastructure): make the edge installable from the cloud,
  runnable on real hardware, self-updating, authenticated, fast and durable.
- **Program B — Complete the Domain** (business): the FnB flow, fraud/RBAC, receipts,
  international tax, retail, integration, the config platform, floor plan, and the
  plug-and-play boundary.

They touch mostly disjoint files, so they proceed as two lanes. Every change to the wire
or the event catalogue is **additive** (the catalogue permits new fields/types, never a
rename or removal); every new behaviour sits behind a **capability flag or a config node**.

## Milestones

| Milestone | Contents | Meaning |
|---|---|---|
| **v1.0 — Ship & Safe** | A·P1→P3 + **A·P1x** + B·W1 + B9.1 (≈31 PR, 21 merged) | Install from the dashboard onto Win/Linux, activate by code, sell offline-safe, real menu from cloud, no LAN auth hole, no inventory bug, tax correct per channel, `/admin` API contract + integrator docs, and the integration doctrine gate landed early. |
| **v1.1 — FnB & Management** | B·W2 + B·W3 + B·W7 + B·W8 + A·P4 + A·PF (≈29 PR) | Full order check → bill check → final bill; void/discount/split/refund with routes, events, ceilings; modifiers/notes/courses/fire-rounds; receipt engine per store/order; which item prints at which kitchen printer; config force/lock/fan-out with per-device drift; multi-floor drag-drop floor plan; alerting, network printing, backup; and the performance gates. |
| **v1.2 — International** | B·W4 + B·W5 + B·W6 + B·W9 + A·P5 (≈19 PR + ops) | Multi-component & inclusive tax → `countries/in` + `countries/jp` demo; tender/denominations/buyer-invoice as data; retail quick-sale by preset; plug-and-play proven by CI (connector framework, third-party KDS over `/ws`, card terminal over the port); pilot on real hardware. After this a Japan and an India store can pilot together on one cloud. |
| **v1.3 — Production International** | B·W10 + JP/IN go-live (≈3 PR code + ops, gated) | Qualified-invoice Japan, IRP e-invoice + UPI India, a real card-terminal adapter, data-residency decision (APPI/DPDP), independent pentest. The code is small; the gate is legal registration and physical devices. |

Rough sequential estimate: v1.0 ≈ 6–8 wk · v1.1 ≈ +8–10 wk · v1.2 ≈ +5–7 wk · v1.3 ≈ +2–3 wk of
code (calendar set by legal/device lead time). The two lanes running in parallel shortens this
materially. **≈82 PR total** plus the gated W10 — up from the original ≈70 by A·P1x's seven closing slices
(E7a split out of E7 once ADR-0089 found it needed a certificate path) and the two posture ADRs
(D23/D24). **24 merged** as of 2026-09-02 (#71–#74, #90–#110): all of A·P1's code, B9.1, R2's ADR and
its release registry, E5, E6, E7a, E7, the two posture ADRs, the `X-Forwarded-For` rate-limit fix,
and the NATS credential fix.

## Program A — Ship the Edge

### A·P1 — Unlock: ship & run
- **S0** — Edge auth (security, first). Validate the paired `DeviceToken` on every domain route and `/ws`; a real actor from pairing + PIN sign-in replaces the hardcoded employee-1 `dev_actor`. Nothing below is meaningful while identity is forged. **Landed as S0a/S0b for the domain routes; `/ws` and durability remain — see S0c/S0d.**
- **C1** — Turn on the two stub CI jobs (contract, soak) + Dependabot. **Done.**
- **R1** — Release workflow: build + minisign-sign (keys in GitHub secrets, never on the VPS) + publish. **Done, but the artifact carries no version — see R1b.**
- **E1** — `cloud_url` into `EdgeConfig` + wire the config-pull and heartbeat loops into `serve()`. **Loops done; the provisioning half is not — see E6.**
- **E2** — OS-keyring `KeyVault` adapter + the `/setup` activation screen in the edge UI. **Done; reachable only once E6 lands.**
- **E3** — Wire the relay client + NATS event publish. **Wired; neither can reach production — see E6 and E7.**

### A·P1x — Close the chain (found by the 2026-09-02 tree audit)

A·P1 shipped every slice's *code* and the wiring from `main.rs` was verified live. What the audit found is
that the **chain** those slices form has never run end to end: a store provisioned by the guided wizard
boots LAN-only, the relay cannot be granted its scope from the console, the event bus is unreachable from
outside the box, and the OTA updater is never constructed. Seven closing slices, all small, all blocking the
first real store.

- **S0c** — `/ws` requires the paired device token. It is mounted on the ungated infra router, outside both
  `require_paired_device` and `require_signed_in`, so any host on the store LAN reads the whole
  committed-event fan-out — orders, bills, settlements. ADR-0084 deferred this to B6.1; the audit rules it
  a live hole, not a deferral. (The read-only *scope* and event-type filter stay with B6.1.)
- **S0d** — Durable pairing and sign-in state. Both live in process memory today, so an edge restart
  re-pairs and re-signs-in every device — mid-service, on a box that is expected to be power-cycled.
- **R1b** — Stamp the release tag into the binary. `crates/pos-edge/Cargo.toml` is `version = "0.0.0"` and
  nothing writes the tag at build time, so every artifact reports the same version and the whole OTA
  progress model (ADR-0078) cannot tell one release from another.
- **E6** — Close the provisioning chain. The wizard's `config.toml` generator emits no `cloud_url` at all,
  so `compose_cloud_surface` never runs and `/api/activation` 404s on a store built exactly as the runbook
  says. Fix: emit `cloud_url`, offer a bind port, and generate the mode-0600 env file
  (`POS_EDGE_SYNC_KEY`, `POS_EDGE_NATS_URL`) beside it; and add `relay_orders` to the key-issuance UI,
  which today offers four scopes and not that one — so no operator can grant the relay its scope.
- **E7a** — TLS becomes a chosen posture, not an inference (D24, [ADR-0090](adr/0090-tls-postures.md)).
  `bootstrap.sh` picks the TLS method from `DOMAIN`'s suffix, which reaches only two of the four
  legitimate postures, silently downgrades a managed domain with an empty `CF_DNS_API_TOKEN` to a method
  that cannot work on a DNS-only record, and records the choice nowhere. An explicit `TLS_MODE`
  (`acme-http01` | `acme-dns01` | `byo-cert` | `external`) selects a committed per-mode Caddyfile instead
  of overwriting one, establishes `secrets/tls/` as the certificate path every consumer reads, and makes
  `trusted_proxy_hops` configuration — without which `external` collapses the whole company onto one
  login-rate-limit bucket. **Sequenced before E7**: ADR-0089 binds TLS to the published port and needs a
  certificate path some posture put there on purpose.
- **E7** — Make the event bus reachable (D23, [ADR-0089](adr/0089-edge-event-bus-transport.md)). NATS sits
  on an `internal: true` Docker network with no published port and no proxy route, so
  `POS_EDGE_NATS_URL` has nowhere valid to point and the outbox publishes nowhere. The store keeps
  trading and the events stay durable, so the failure is silent: the cloud simply never receives
  anything, and rollups, reports and reconciliation all read empty. **Follows E7a** for the
  certificate. **Done**: `nats` publishes `4222` with TLS from `secrets/tls/`, and the port opens
  only when a certificate is there — a first ACME deploy leaves it closed on purpose. Two corrections
  came out of it: the URL never carried its token (`async-nats` reads credentials only from its
  connect options, so `link-nats` now lifts them) and the integration suite had been running against
  a broker with no authorization at all. Reachability of a published port on an `internal: true`
  network stays **unverified** — a real box has to say, and the fallback is recorded.
- **R5** — Wire `OtaUpdater` into the running edge. It has **zero production callers** — the only
  construction in the tree is `crates/pos-edge/tests/ota.rs`. Four merged slices (P9a/P9b/P9e-4, ADR-0047/
  0048/0055) built update decision, signature verification, self-test and rollback, and none of it runs.
  Principle 3 ("dễ cập nhật") is unmet until this lands, and it must land with R4's real installer.

**Q1 moves up.** The in-process end-to-end acceptance suite is listed under A·P3, but it is the gate that
would have caught every one of the seven above — seven times this program has merged code that was written,
tested and unreachable. Q1 runs immediately after A·P1x, before any new A·P2 or Program B slice.

### A·P2 — Publish from the cloud
- **R2** — OTA artifact server + Garage store + promote-release; the cloud stays a dumb host, the edge verifies the signature ([ADR-0088](adr/0088-ota-artifact-hosting.md)). **The release registry landed (`pos_cloud::ota`, migration 0038); nothing reads it yet.** Remaining, in order: artifact storage over `BlobStore` (adds `blob-garage` to `pos-cloud` plus S3 credentials the deployment does not provision — ADR-0088 Correction 1), the `POST /internal/ota/artifact` route with the additive `arch` field the pinned request lacks (Correction 2), the adapter's bearer, then `/admin` upload + promote-release with audit and the runbook step.
- **R3** — Per-store installer on the Handoff screen; one-file `pos-edge install --store <id>` self-installer; zip is the fallback. **Depends on E6** — an installer that writes a `config.toml` without `cloud_url` reproduces the same break at scale.
- **E4** — Windows service wrapper.
- **E5** — Edge UI consumes the **real** menu/locale/tender from `EdgeSession` (kills the hardcoded `ui/src/lib/menu.ts`; publishing a menu now changes the POS). Also: the UI never computes money — every figure comes from the edge. **Done, less three residual hardcoded `VND` sites**: the shift opening float, the shift screen's expected-cash figure, and one Pay-screen label.
- **R4** — Real `UpdateInstaller` for Linux (systemd swap → self-test → rollback); Windows follows E4. **Ships with R5** — an installer with no caller is the same gap again.

### A·P3 — Prove
- **Q1** — In-process end-to-end acceptance suite (dine-in + takeaway), the v1.0 gate.
- **Q2** — i18n-parity gate + dashboard code-split.
- **Q3** — AIP-193 envelope on `/v1` **and** `/admin` (both return plain text today) + ETag/If-Match.
- **Q4** — Store hub + URL context `/t/:tenant/s/:store`.
- **Q5** — `/admin` becomes a real contract: pagination/`q`/sort on the unbounded lists; `/admin` into OpenAPI + the drift gate; fix the webhook header docs↔code mismatch; implement or drop `pos-api-version`; wire or delete the two dead scopes; rate-limit `/v1/orders` and `/sync`.
- **Q6** — Integrator docs: webhook quickstart (correct HMAC header), auth guide, API tour.
- **Q7** — UX step budget: a measured action budget for ~12 common tasks, failable in e2e (add item ≤2 taps, cash settle ≤3, price change ≤4 clicks).

### A·P4 — Operate
- **O1** — Alert delivery webhook.
- **O2** — TCP `:9100` printer transport + a "test print" button.
- **O3** — Store-side WAL-shipping backup.
- **O4** — JetStream capacity probe.
- **O5** — In-code auth for `/internal/*` (a shared-secret header; defence in depth over the network boundary).

### A·PF — Performance & durability (turns principle 4 into gates)
- **PF1** — Latency budget as a CI gate: p99 per operation on a standard weak box (2-core/2GB) — add item <30 ms, fire <40 ms, settle <60 ms, `/ws` fan-out <50 ms (kept from P5), config apply <200 ms. Criterion + in-process HTTP benches; a regressing PR goes red.
- **PF2** — Real CCU load test: one edge under 30 devices + 200 QR sessions + vendor intake at peak; cloud under 500 stores long-polling + NATS ingest. Nightly 8-hour soak with p99 + zero-loss thresholds.
- **PF3** — Resource ceilings: edge RSS <300 MB, idle CPU <5 %, edge-UI bundle <250 kB gzip; SQLite/WAL tuning; dashboard code-split.
- **PF4** — Long-run durability: SNTP `ClockSource` + clock-drift alert; prune synced events (the log must not grow without bound); disk-space guard + early alert.

### A·P5 — Pilot (ops, gated)
- **P1** — WS-F security review + human/hardware gate register.
- **P2** — Real-hardware pilot (including sudden power loss) + soak at 222 ev/s.

## Program B — Complete the Domain

### B·W1 — Correctness first
- **B1.1** — Fix fire zeroing modifiers (thread modifiers through the line draft/record/event so recipe consumption is correct) + inventory regression test.
- **B1.2** — Sales channel is a per-order attribute, not per-session (correct tax per order; dine-in emits `sales.order.opened` with channel/guest count).
- **B1.3** — Real tips (drop the hardcoded zero; record per-payment; gate on `tips_enabled`).

### B·W2 — Full FnB flow
- **B2.1** — Pre-bill (bill check): effect + `billing.prebill.printed` event + projection marker + flag `pre_bill` (D9: an effect, not a new state).
- **B2.2** — Void line / void bill routes end-to-end with manager PIN + reason codes.
- **B2.3** — Discount / comp / price-override / refund routes with real ceilings in the `Grant`; over-ceiling emits `security.permission.overridden` with the approver.
- **B2.4** — Split / merge / transfer routes + Pay UI (the pure functions exist and are property-tested; only routes/events/screens are missing).
- **B2.5** — Modifiers/toppings on the dine-in path (additive line/event fields; min/max enforced from the modifier group; KDS shows them). *B1.1's proper home for the plumbing.*
- **B2.6** — Real note text + serving style (note text kept local/PII-safe; serving style an open line attribute).
- **B2.7** — Course entity + fire-round + kitchen-ticket print.

### B·W3 — Receipt engine, flexible per store/order
- **B3.1** — Renderer `BillTotals` → `PrintDocument` with a document-kind field (kitchen ticket · order check · pre-bill · final receipt · shift report); prints per-class tax lines.
- **B3.2** — Structured `receipt` config node (sections on/off, header/footer/promo, logo media id, per-channel variants; not a DSL — D10).
- **B3.3** — Bitmap rasterizer (embedded Noto subset per locale) so Vietnamese/Japanese/Hindi print on any ESC/POS printer; reprint stamps COPY + emits an event + enforces the reprint permission.
- **B3.4** — Bind station ↔ printer device + print dispatcher + KDS-per-station. Routing item→station already works; this connects station → physical print (no field binds a station to a printer, nothing builds a `PrintJob`, KDS shows every line). Answers "which item prints at which kitchen printer" end to end.

### B·W4 — International tax & country v2
- **B4.1** — Multi-component tax: `rate_for → Vec<TaxComponent>` (CGST+SGST, GST+PST); tax lines per component; settled event carries the breakdown. Done once in core.
- **B4.2** — Tax-inclusive + rounding from config: `prices_include_tax` flag + back-out math; rounding mode + cash-rounding increment + service charge read from a node, not hardcoded.
- **B4.3** — `CountryModule` trait v2 + HSN/SAC: hooks for tax semantics, receipt blocks, item-tax-code validation; `Fiscalization` stays provider-agnostic.
- **B4.4** — `countries/in` + `countries/jp` (proof): India GST split + lakh number format; Japan JPY + inclusive + 8/10 per channel (works because of B1.2).
- **B4.5** — Tender is **data**, not code: per-method metadata (kind class, opens drawer?, needs terminal?) + quick-cash denominations per currency (Pay.tsx hardcodes VND today).
- **B4.6** — Generic buyer/company invoice info + per-country tax-id validation (VN MST · India GSTIN · EU VAT); receipt and `Fiscalization` both read it.

### B·W5 — Retail sells + flags gate
- **B5.1** — SKU / barcode / PLU on the item (additive + index + compiler + fast edge lookup).
- **B5.2** — Tableless quick-sale: `POST /api/orders` + a Sell screen (search/scan → cart → pay-first), the same Order/Bill machine; every format is a preset (D12).
- **B5.3** — Capability enforcement pass: all 10/10 flags gate real behaviour + new flags (service_charge, pre_bill) + new rules; fix the counter/retail presets so a store can actually sell.

### B·W6 — Integration hub
- **B6.1** — `/ws` read-only scope + event-type filter, so a third-party KDS plugs in <50 ms on the LAN. **The authentication half moved earlier to S0c** — an unauthenticated socket is a live hole, not an integration feature, so it is not waiting for this wave.
- **B6.2** — Cloud webhooks: event-type filter.
- **B6.3** — `vendor_id` + external↔internal item map + generic delivery address on intake.
- **B6.4** — Wire `DeliveryVendor` + live `vendors` node + staff-confirm release; device gains an address field (pre-configure a printer IP).
- **B6.5** — Production realtime contract: a read-only GET snapshot for integrators + monotonic sequence on `/ws` + `?from_seq=` resume from the durable event log + a versioned contract doc. (Today `/ws` is a live fan-out with no replay — a reconnecting KDS loses events.)

### B·W7 — Config platform v2 (cloud is the single source, with force, with visible drift)
- **B7.1** — Shared tenant/brand layers + real fan-out (today the tree is per `(tenant, store)`, so publishing "tenant" touches one store).
- **B7.2** — Lock/force by path (a brand pins `tax`; a lower layer overriding a locked path is 409'd; the UI shows a lock).
- **B7.3** — Drift v2: persisted applied version, per-device, surviving restart; heartbeat carries telemetry; rollback keeps layer attribution.
- **B7.4** — Settings UX per layer (typed form per node at every level, showing the effective value, where it is inherited from, and whether it is locked).
- **B7.5** — Read-only Settings/Diagnostics screen on the edge (effective config + provenance + version gap). Edge sees and reconciles; changes still go through the cloud (D18).

### B·W8 — Multi-floor drag-drop floor plan
- **B8.1** — Floor/storey entity (a store has floors; a floor has areas; an area has tables).
- **B8.2** — Drag-drop floor designer on the dashboard (snap to `GridPosition`; toggle "plan ⇄ table list"; the form CRUD remains the accessible fallback — D17).
- **B8.3** — Edge draws the real plan per floor (the authored `GridPosition` is currently ignored — the edge flattens to a card grid).
- **B8.4** — Live pax/covers (party size on seat; covers into `sales.order.opened`; average check per cover). `seats` is authored today and read by nobody.

### B·W9 — Plug-and-play marketplace
- **B9.1** — Integration doctrine (ADR-0083 — the three plug points) + a CI gate keeping vendor names out of `pos-core`/`pos-proto`. Lands early to guide every later wave.
- **B9.2** — Wire the `PaymentTerminal` port (it exists, designed well — `Unknown` a first-class result, no card data ever — but the edge calls it zero times). Card is only "marked paid" today.
- **B9.3** — Connector framework for order intake: a thin per-vendor connector on the cloud receives *their* webhook, verifies *their* signature, and transforms to the internal idempotent `/v1/orders`. A new marketplace is one connector, not a core change.
- **B9.4** — Plug-and-play proof kit: a sample KDS client (uses only the public `/ws` contract, dogfooded) + a sample connector running in the e2e suite, so "any system can integrate" is CI-verified.

### B·W10 — Country hardening (gated: legal registration + real devices)
- **B10.1** — Japan production pack: qualified-invoice receipt block (registration number, per-invoice 8/10 breakdown), Japanese UI/receipt translations, JPY denominations, inclusive/rounding acceptance suite.
- **B10.2** — India production pack: IRP e-invoice adapter over `Fiscalization` (IRN + signed QR; needs GSP/sandbox registration), UPI QR on the receipt + confirmation flow, GSTR export.
- **B10.3** — A real card-terminal adapter for the pilot country, into the `PaymentTerminal` port wired at B9.2.
- **B10.4** — Data-residency + legal + pentest (ops): hosting-region decision (APPI/DPDP), independent pentest after WS-F.

## Debates settled

The full debate log (D1–D22) lives in the planning artifact. The load-bearing ones:

- **D1** — CI builds and signs; the cloud distributes. Signing keys never touch the VPS.
- **D5** — Live server→edge push stays deferred pending pilot latency data; long-poll is production-adequate.
- **D9** — Pre-bill is an effect + event + marker, not a new bill state.
- **D10** — Receipts are a structured config node, not a template DSL.
- **D11** — Tax is generalised in core once (multi-component + inclusive + rounding) before any country is added.
- **D12** — One Order/Bill machine; FnB/retail/counter/drink-shop differ only by capability preset.
- **D13** — Edge auth (S0) is not deferrable; it comes first.
- **D18** — The cloud is the single source of config; the edge sees and reconciles but does not write.
- **D19** — Core keeps only the POS invariant; every external system plugs in through one of three points (ADR-0083).
- **D20** — Beyond tax, tender/denominations, buyer-invoice, delivery address and number formatting are all generalised to data/hooks, not per-country branches.
- **D21** — A tenant is a legal entity; country/currency/timezone are attributes of a *store*. A tenant typically maps to a country because fiscal credentials live on the legal entity.
- **D22** — Non-functional promises (fast, light, few steps) are protected by CI gates, not prose.
- **D23** — The edge reaches NATS **directly over TCP**, not through the HTTP reverse proxy. Both were viable and performance-indistinguishable at this workload — the publisher drains the outbox in batches every 5 s, ~135 events ≈ 40–100 KB, so ~20 KB/s per store, nowhere near any transport's limit, and WebSocket's per-frame mask is a rounding error there. Direct TCP wins on four things a proxy cannot give: TLS terminates *at NATS*, which is the only way to authenticate a store by **client certificate** (through a proxy the identity dies at the proxy — a header NATS will not read); NATS's own cluster gossip lets clients failover across nodes, which a proxy hides behind internal addresses; the proxy is not a shared chokepoint for both the console and the bus (Caddy runs on `cpus: 0.25` / `mem_limit: 96m` and already carries the long-polls); and there is no upgrade round-trip per reconnect, which matters for the deferred live mode (ADR-0062). It also needs **no** proxy plugin: the `nats` container publishes `4222` itself and Caddy is not in the path. **Revisit if** the fleet outgrows one NATS node, per-store mTLS is dropped, the bus starts carrying large payloads (the deferred log tail), or the proxy measurably stops being the constraint — the transport is a URL, so reversing costs a config line, not code (`async_nats::connect` takes either scheme from the same binary).
- **D24** — TLS termination is a **fork-level posture, not a fixed choice**. Four are legitimate and a framework must serve all four: ACME HTTP-01 (the default; sslip.io and any A-record domain), ACME DNS-01 (a Cloudflare-managed domain, grey-clouded), **bring-your-own certificate files** (a company with a wildcard or an internal CA and no ACME), and **termination upstream** (a company whose own load balancer, ingress or tunnel already does TLS and where the bundled proxy should do none). An explicit `TLS_MODE` selects a committed per-mode file; nothing overwrites one. **Recorded in [ADR-0090](adr/0090-tls-postures.md)**, which also establishes `secrets/tls/` as the single certificate path — the dependency ADR-0089 was waiting on. The `external` mode is the one with a hidden edge: the app must then trust `X-Forwarded-For`/`X-Forwarded-Proto` from *that* balancer, or the login rate limit collapses every user onto one source IP and one wrong password locks the whole company out.
- **D25** — For mTLS on the bus, the **server certificate and the client CA are different trust decisions**. The server certificate may be public (ACME or brought). The CA that verifies *store* certificates must be **private and ours** — configure a public CA there and anyone who can obtain a certificate from it can speak to the bus, which is the most common way mTLS is misconfigured into a no-op. Store certificates carry the `store_id` as their subject and NATS maps it (`verify_and_map`), which is what finally gives a box a real fleet identity rather than one derived from its store id. Custody follows D1's reasoning: a CA key on the VPS means owning the VPS is owning every store's identity, so the pilot may generate it on the box (documented as a pilot posture) and a fleet moves it offline before scale.

## Cadence

One track is one pull request. Every slice runs the full gate set — `cargo fmt --all -- --check`,
`cargo clippy -p <crate> --all-targets --all-features -- -D warnings`, `cargo test`, the relevant
`cargo run -q -p xtask -- <check>`, and `cd dashboard && pnpm build` for dashboard work — carries a
CHANGELOG `[Unreleased]` entry, and opens with an ADR when it introduces or changes an architecture.

**Name the call site.** Every pull request must state, in its body, the production call site that makes
its code reachable: the line in `main.rs` or `serve()` that constructs it, the router that registers its
route, or the screen a UI router mounts. "The module exists and its tests pass" is not that. This program
has merged fully written, fully tested, completely unreachable code **seven** times — the order relay, the
outbox publish, the OTA artifact route, the OTA updater itself, the event-bus transport, `/ws`
authentication, and the provisioning `cloud_url` — and in every case the tests were green, because a test
supplies its own caller. A slice whose call site is "none yet, by design" (a storage seam ahead of its
route, say) says so explicitly and names the slice that will call it; anything else is not done.
