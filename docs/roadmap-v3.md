# Roadmap v3 — Production-ready, international, plug-and-play

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-04
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
| **v1.0 — Ship & Safe** | A·P1→P3 + **A·P1x** + B·W1 + B9.1 | Install from the dashboard onto Win/Linux, activate by code, sell offline-safe, real menu from cloud, no LAN auth hole, no inventory bug, tax correct per channel, `/admin` API contract + integrator docs, and the integration doctrine gate landed early. |
| **v1.1 — FnB & Management** | B·W2 + B·W3 + B·W7 + B·W8 + A·P4 + A·PF | Full order check → bill check → final bill; void/discount/split/refund with routes, events, ceilings; modifiers/notes/courses/fire-rounds; receipt engine per store/order; which item prints at which kitchen printer; config force/lock/fan-out with per-device drift; multi-floor drag-drop floor plan; alerting, network printing, backup; and the performance gates. |
| **v1.2 — International** | B·W4 + B·W5 + B·W6 + B·W9 + A·P5 (+ ops) | Multi-component & inclusive tax → `countries/in` + `countries/jp` demo; tender/denominations/buyer-invoice as data; retail quick-sale by preset; plug-and-play proven by CI (connector framework, third-party KDS over `/ws`, card terminal over the port); pilot on real hardware. After this a Japan and an India store can pilot together on one cloud. |
| **v1.3 — Production International** | B·W10 + JP/IN go-live (small in code; ops-gated) | Qualified-invoice Japan, IRP e-invoice + UPI India, a real card-terminal adapter, data-residency decision (APPI/DPDP), independent pentest. The code is small; the gate is legal registration and physical devices. |

### v1.0 code-complete (recomputed 2026-09-04, item by item against the tree)

v1.0 is `A·P1 → A·P3` + `A·P1x` + `B·W1` + `B9.1`. The nine that were open on 2026-09-04 closed that
day:

| # | Slice | What landed |
| --- | --- | --- |
| 1 | **B1.1** | Modifiers thread through the line draft, record and event, so recipe consumption is right. |
| 2 | **B1.2** | Channel is per-order. Three defects, not one: `seat_table` emitted no `sales.order.opened` at all, the replay ignored the event (so the fix evaporated on restart), and the bootstrap tax table covered only dine-in — which would have stopped a LAN-only store settling a takeaway order it had already fired. |
| 3 | **B1.3** | Real tips, gated on `tips_enabled`. Also worse than named: each payment's `change_given` was `tendered − applied`, so the till over-reported change by exactly the tip — it told the cashier to hand back money the guest had just left. |
| 4 | **E5 residual** | Four sites, not three. The unnamed one was the only *wrong number* rather than a wrong label: `parseWhole(amount(), "VND")` is out by a factor of 100 on any two-decimal currency. |
| 5 | **Q5 tail** | Two of the four documented webhook headers described a per-event delivery model that was never built, so this was not a rename. `pos-api-version` dropped (nothing read it), the two dead scopes deleted (`POST /admin/api-keys` still *accepted* them, so a key could promise authority no route consults), and `/v1/orders` + `/sync` given budgets — per-tenant after auth for orders, per-connection before auth for sync, because the store id on `/sync` is caller-supplied and keying on it would let anyone exhaust one named shop's budget. |
| 6 | **Q6** | [`docs/guides/integrate-with-the-api.md`](guides/integrate-with-the-api.md): auth, an API tour, webhooks with a complete receiver, and an explicit list of what is deliberately not there. |
| 7 | **Q7** | `ui/scripts/step-tasks.mjs` declares fifteen selling tasks; `ui/scripts/step-budget.mjs` resolves every declared tap against `App.tsx` and the screen that renders it, run by `pnpm build`. The one case it cannot see — a required tap nobody declared — is now closed by `ui/tests/replay.spec.mjs`, which clicks the same taps in a browser against a real edge and asserts each flow reaches its outcome ([ADR-0109](adr/0109-counting-the-taps-an-operator-makes.md)). The console half followed: `dashboard/scripts/step-budget.mjs` measures five office flows the same way and reports rather than rules, because the till's two-and-three-tap ceiling is not a console's — so it has no ceiling for a browser to defend yet. |
| 8 | **R3** | A per-store `install-pos-edge.sh` on the wizard's Handoff screen, embedding both files, laying out the update slots, and deliberately leaving an already-installed binary alone so a box that updated itself is not rolled back. |
| 9 | **E4** | The Windows service wrapper (`crates/pos-edge/src/service.rs`). The old instructions could not have worked — a console program never answers SCM's start handshake — and OTA could not have worked even with a shim, because SCM treats a clean exit as a deliberate stop, so a store that installed a release would have gone dark. `ServeOutcome` now says whether a stop was a restart, and the wrapper turns that into an exit code a failure action acts on. |

**Then auditing those nine against the tree found five of them half-landed**, and the reason is worth
keeping: each was reported done on the strength of the half that shipped. The domain change and the
operator change are separate files, often separate languages, and a slice whose domain half is
written, tested and merged *looks* finished from the inside.

| Slice | The half that was missing | Now |
| --- | --- | --- |
| **B1.3** | `Payment.tip` reached the edge and was recorded; the till had no tip entry, so the amount was zero on every real payment | **Closed** (#183) |
| **E5** | A fifth hardcoded-`VND` site in `Takeaway.tsx` — the same defect fixed one file over, written the same way and missed | **Closed** (#183) |
| **E3** | The wizard emitted no `[nats]` section, so every provisioned store published nothing at all | **Closed** ([ADR-0087](adr/0087-edge-relay-and-event-publish.md) Amendment 1 settles the stream layout the value needed) |
| **Q4** | The URL context half shipped; the per-store landing screen it was for did not | **Closed** — six cards on the tenant-scoped index ([ADR-0099](adr/0099-store-hub.md)); Reports moved to `/reports` |
| **Q7** | `ui/` got its step budget; `dashboard/` has none, so the console's own flows are unmeasured | **Closed** — `dashboard/scripts/step-budget.mjs` resolves five console flows against the source; the price-change flow measures **8** clicks, and the ceiling is the owner's to set from that ([`ui-ux.md`](ui-ux.md) §1.6) |

**What is not done is everything that needs a machine.** [`gate-register.md`](gate-register.md) §6 is
the list, and E4 narrowed one row rather than closing it: the wrapper is compiled by the
`windows-2022` CI job, but that a real service reaches `RUNNING`, drains on stop and restarts on
exit `1` is a Windows box's job to prove. The same is true of the `systemd` restart, the headless
keyring across a reboot, power loss mid-transaction and the 222 ev/s soak.

The next code is **v1.1** — `B·W2` + `B·W3` + `B·W7` + `B·W8` + `A·P4` + `A·PF`, of which A·P4 has
two of five open (`A·P4 O2` printer transport, `A·P4 O3` store-side WAL shipping) and all four of
A·PF is open.

Rough sequential estimate: v1.0 ≈ 6–8 wk · v1.1 ≈ +8–10 wk · v1.2 ≈ +5–7 wk · v1.3 ≈ +2–3 wk of
code (calendar set by legal/device lead time). The two lanes running in parallel shortens this
materially.

### Why this roadmap no longer estimates in pull requests

It used to. The table above carried a per-milestone PR count and the total read **≈82 PR**, with
**25 merged**. Both numbers are gone, and the reason is worth keeping.

**The total was passed while v1.0 was still open.** As of 2026-09-04, **85 pull requests have merged
since this roadmap landed** — three more than the whole four-milestone estimate — and v1.0 is not
finished. The estimate was not a little optimistic. It was measuring the wrong thing.

**What it was actually measuring is slice granularity.** The count assumed one PR per named slice —
`S0`, `E1`, `R2`, `Q3`. The tree's review rule is one reviewable behaviour per PR, so a named slice
routinely becomes several. `Q3` is one bullet under A·P3 and **twenty-two** merged PRs
(#120, #131–#143, #145–#147, #150–#154). ADR-0098's paging work is eight. Neither overran; both were
sliced to be reviewable. A number that moves when you change how finely you slice is not a measure of
remaining work, and every hour spent maintaining it bought nothing a reader could act on.

**The merged figure was also wrong on the day it was written** — it said 25 where its own stated range
(#71–#74, #90–#112) contains 27. Nothing caught it, because nothing could: a hand-typed count has no
source to be checked against.

So this roadmap states progress in slices, which it can name and whose status it already carries
inline (**Done**, **Depends on**, **Ships with**). If you want the PR figure, recompute it rather than
trusting a sentence:

```sh
gh pr list --state merged --limit 500 --json number \
  --jq '[.[] | select(.number >= 71)] | length'
```

Everything from **#71** (B9.1, which landed this roadmap) onward belongs to the v3 era. **#75–#89 are
not merged work** — they exist, as fifteen Dependabot pull requests that are still open, which is why
the recompute above counts only merged pull requests. (An earlier revision of this paragraph said they
"do not exist"; the 2026-09-04 inventory that checked every number found them, along with the fact
that #2–#26 are issues — GitHub numbers issues and pull requests from one sequence — and that **no
pull request in this repository has ever been closed unmerged**.) What is *in* the merged set is not
derivable from the number, which is the other half of why it was never useful on its own — read
`CHANGELOG.md` for that.

## Program A — Ship the Edge

### A·P1 — Unlock: ship & run
- **S0** — Edge auth (security, first). Validate the paired `DeviceToken` on every domain route and `/ws`; a real actor from pairing + PIN sign-in replaces the hardcoded employee-1 `dev_actor`. Nothing below is meaningful while identity is forged. **Landed as S0a/S0b for the domain routes and S0c for `/ws`; durability remains — see S0d.**
- **C1** — Turn on the two stub CI jobs (contract, soak) + Dependabot. **Done.**
- **R1** — Release workflow: build + minisign-sign (keys in GitHub secrets, never on the VPS) + publish. **Done**; the artifact now carries its version too (R1b).
- **E1** — `cloud_url` into `EdgeConfig` + wire the config-pull and heartbeat loops into `serve()`. **Loops done; the provisioning half is not — see E6.**
- **E2** — OS-keyring `KeyVault` adapter + the `/setup` activation screen in the edge UI. **Done; reachable only once E6 lands.**
- **E3** — Wire the relay client + NATS event publish. **Done.** The loops landed here, E6 gave them a
  cloud to reach and E7 gave the bus a published TLS port — and the last gap was the quietest of the
  three: nothing generated the `[nats]` section, so every store the console provisioned ran the
  publish loop's early return and shipped none of its sales. Closing it needed a decision the tree
  did not contain: three doc comments said one stream per store while `pos_cloud` binds one durable
  consumer to one named stream. [ADR-0087](adr/0087-edge-relay-and-event-publish.md) Amendment 1
  settles it — one fleet stream, one subject, identical on every box — and says why the *subject*
  being shared is the load-bearing half rather than a tidiness choice.

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
  **Done** ([ADR-0084 amendment](adr/0084-device-authentication.md)): a one-route sub-router carries
  `require_paired_device_ws`, which accepts the token from `Authorization` **or** from
  `Sec-WebSocket-Protocol` — the browser `WebSocket` API cannot set a header, and a query parameter was
  rejected because the edge logs the request path. The server selects only the protocol *name*, so the
  credential never comes back in the handshake. `/healthz`, `/api/pair` and the asset fallback stay open by
  necessity.
- **S0d** — Durable pairing and sign-in state. Both live in process memory today, so an edge restart
  re-pairs and re-signs-in every device — mid-service, on a box that is expected to be power-cycled.
  **Framed by [ADR-0091](adr/0091-durable-edge-auth-state.md)**, which had to precede it: every adapter
  depends on exactly `pos-proto` + `pos-ports`, so a trait `store-sqlite` implements has to live in
  `pos-ports` — S0d is the **eighteenth port**, `DeviceRegistry`, and therefore an ADR first. The
  record also settles what persistence *changes*: the token is stored as a SHA-256 digest so a stolen
  `pos.db` yields no working credential; revocation becomes explicit, because a restart stops being
  the accidental revocation; and **both** tables survive a restart, with a 30-minute
  `sign_in_idle_timeout` carrying the risk that durable sign-in creates.

  Split in two, the way [ADR-0053](adr/0053-cloud-sync-port.md)'s `CloudSync` was (port+suite+fake,
  then adapter, then edge wiring):
  - **S0d-1 — the port. Done.** `DeviceRegistry` in `pos-ports` with `TokenDigest` (hand-rolled hex,
    because `pos-ports` is backbone and carries no hash dependency), `PortName::DeviceRegistry` as the
    eighteenth, a 13-case contract suite, `FakeDeviceRegistry`, and the `store-sqlite` adapter with
    additive migration `0005_device_registry.sql`. Both implementations pass the same 13 cases, so
    "swappable" is checked rather than claimed. No behaviour change in `pos-edge` yet.
  - **S0d-2 — the edge wiring. Done.** `Pairing` and `Sessions` are write-through over the port with
    in-memory reads, both tables load at boot (fatal if unreadable — starting empty would silently
    unpair a store that *is* paired), `has_gone_idle` is a pure policy that fails closed on a stepped
    clock, `sign_in_idle_timeout_minutes` is configurable at 30, and `/api/pair/revoke` +
    `/api/pair/devices` join the pairing surface behind its own gate. The object-safe seam lives in
    `pos-edge`, **not** `pos_ports::dynamic` — that module reserves mirrors for runtime selection, and
    this is the chosen-once-at-startup kind — so `serve`'s signature and `main.rs` are unchanged. One
    property fell out for free: keying the live map by digest as well means the edge now holds no
    device token anywhere.

  **Done, both slices.** `DeviceRegistry` is the eighteenth port with its own contract suite, `pos-fakes` implementation and `store-sqlite` adapter; the edge writes through to it on pair and on sign-in, refills both tables at start-up, and carries the `sign_in_idle_timeout` as configuration (`crates/pos-edge/src/durable_auth.rs`, and the `DeviceRegistry` bound on `compose`). A power cycle no longer unpairs the shop.
- **R1b** — Stamp the release tag into the binary. `crates/pos-edge/Cargo.toml` is `version = "0.0.0"` and
  nothing writes the tag at build time, so every artifact reports the same version and the whole OTA
  progress model (ADR-0078) cannot tell one release from another. **Done**: the release workflow exports
  `POS_EDGE_RELEASE_VERSION` from the tag it already validates, and `pos_edge::version` reads it through
  `option_env!` with `CARGO_PKG_VERSION` as the fallback — so a hand-built binary still says `0.0.0`,
  which is true, and no build script or new dependency is involved. The tag's `v` is stripped because
  `ReleaseVersion::parse` rejects it and the cloud publishes `target_version` bare; a test running
  against the *compiled-in* value fails the build, rather than the fleet, if a fork changes that
  expression. It also pinned the property the rollout gate rests on: `decide_rollout` compares
  `ReleaseVersion`, not text, and there is now a case at each two-digit boundary (9→10, 1.9→1.10,
  1.1.9→1.1.10) — the exact places a string-typed version would silently strand every store.
- **E6** — Close the provisioning chain. The wizard's `config.toml` generator emits no `cloud_url` at all,
  so `compose_cloud_surface` never runs and `/api/activation` 404s on a store built exactly as the runbook
  says. Fix: emit `cloud_url`, offer a bind port, and generate the mode-0600 env file
  (`POS_EDGE_SYNC_KEY`, `POS_EDGE_NATS_URL`) beside it; and add `relay_orders` to the key-issuance UI,
  which today offers four scopes and not that one — so no operator can grant the relay its scope.

  **Done.** The wizard emits `cloud_url` and a bind port, generates the mode-0600 env file beside the `config.toml`, and `relay_orders` is offered in the key-issuance UI — so the loops E1 and E3 built can reach a cloud, which is what made both of those read "wired but unreachable".
- **E7a** — TLS becomes a chosen posture, not an inference (D24, [ADR-0090](adr/0090-tls-postures.md)).
  `bootstrap.sh` picks the TLS method from `DOMAIN`'s suffix, which reaches only two of the four
  legitimate postures, silently downgrades a managed domain with an empty `CF_DNS_API_TOKEN` to a method
  that cannot work on a DNS-only record, and records the choice nowhere. An explicit `TLS_MODE`
  (`acme-http01` | `acme-dns01` | `byo-cert` | `external`) selects a committed per-mode Caddyfile instead
  of overwriting one, establishes `secrets/tls/` as the certificate path every consumer reads, and makes
  `trusted_proxy_hops` configuration — without which `external` collapses the whole company onto one
  login-rate-limit bucket. **Sequenced before E7**: ADR-0089 binds TLS to the published port and needs a
  certificate path some posture put there on purpose.

  **Done.** `TLS_MODE` selects one of four committed per-mode Caddyfiles rather than overwriting one, `secrets/tls/` is the path every consumer reads, and `trusted_proxy_hops` is configuration — without which `external` collapses the whole company onto one login-rate-limit bucket.
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
- **R5** — Wire `OtaUpdater` into the running edge. **Done.** It had **zero production callers** — the only construction in the tree was `crates/pos-edge/tests/ota.rs` — and four merged slices (P9a/P9b/P9e-4, ADR-0047/0048/0055) had built update decision, signature verification, self-test and rollback with none of it running. All four measured slices have landed: **R5-a** the two store-called `/internal` routes moved to `/sync` (the proxy denies that prefix off-box, so *both* the artifact fetch and the update report were unreachable, and the report gained the store-scoped route ADR-0097 said it owed); **R5-b** `arch` stamped from Cargo's `TARGET` by a build script; **R5-c** the real Linux `UpdateInstaller` (= **R4**); **R5-d** the loop, smaller than it looked because `ota_state::device_state` already assembled the whole `DeviceState`. Three things earlier notes listed as blockers were already built: the transport's bearer (`CloudHttpClient` attaches it), response headers on the client, and the edge reading both OTA config nodes. Principle 3 ("dễ cập nhật") is met. What is still gated is the real box: that `systemd` restarts into the retargeted symlink and the store comes back trading (`docs/gate-register.md`), and the Windows installer, which is **E4**.

**Q1 moves up.** The in-process end-to-end acceptance suite is listed under A·P3, but it is the gate that
would have caught every one of the seven above — seven times this program has merged code that was written,
tested and unreachable. Q1 runs immediately after A·P1x, before any new A·P2 or Program B slice.

**Two more of the same, found after that was written.** The eighth: `cloud-sync-http`'s `fetch_update`
POSTs to `POST /internal/ota/artifact`, and `pos-cloud` serves `/internal/ota/report` and **no artifact
route at all** — so R5 would wire the fleet to a loop that reliably 404s. R5 therefore sits behind R2's
artifact-storage slice, and R5's own scoping turned up three further gaps. Two are **the artifact's trust
chain and are now framed by [ADR-0092](adr/0092-artifact-trust-chain.md)**: `UpdatePlan.signature` had no
production producer (`CloudSync` exposed no way to fetch the `.minisig`, which made closing it a port
change and therefore ADR-first — the record amends `fetch_update` to return `SignedArtifact { bytes,
signature }`, so *skipping verification stops being expressible* rather than merely discouraged, with the
signature riding a response header because base64-ing a 30 MB body to carry a few hundred bytes is the
wrong fleet-wide trade); and `trusted_keys` had no source anywhere — now compiled in through `option_env!`
like R1b's release version, with a **prohibition** on any runtime path supplying them, because a key taken
from the cloud-published config tree is a key an attacker controlling the cloud can choose, and a trust
anchor cannot live inside the channel it protects. The third is separate and still open:
`DeviceState.last_self_test` is never persisted although an install deliberately reboots the box, so the
highest-precedence safety rule in `decide_rollout` depends on the one fact a restart loses.

**The tenth was the enforcement machinery itself, and it is now closed.** `IntakeLedger` (ADR-0064) was
a port with no `PortName` variant, so it had no contract suite, no row in `docs/architecture.md` §5, and
its failures were labelled `order_in`. It now has all three plus a six-case suite that `pos-fakes` and
`store-sqlite` both pass — the first time the ledger's two implementations have been checked against each
other. The blind spot that hid it stays on the record: `every_port_has_a_suite` iterates `PortName::ALL`,
so it can only check what was registered, and what should have caught this is the ADR-first rule plus a
reviewer — a process control, not a test.

**The ninth — a takeaway order cannot be paid for. Found by writing Q1.** `EdgeOrderIn` accepts a relayed
order, reprices it from the store's own menu, stores it transactionally and issues a queue number. Then it
stops: `Edge::open_bill` is the only path to a bill, it takes a `TableId`, it gates on that table being
`Occupied`, and it resolves the order with `order_for_table`. A takeaway order is tableless by design
(PR-1b), so no bill can ever be opened on one, no HTTP route opens a bill without a table, and the edge UI
has no takeaway screen at all. **Takeaway revenue is uncollectable at the counter.** Closing it means a
bill keyed on an order rather than a table — a domain change, so it is its own slice with an ADR, not a
patch to the acceptance suite. Q1 asserts the reachable truth and records the gap in the test's own doc.

### A·P2 — Publish from the cloud
- **R2** — OTA artifact server + Garage store + promote-release; the cloud stays a dumb host, the edge verifies the signature ([ADR-0088](adr/0088-ota-artifact-hosting.md)). **Done.** The store-facing route is `POST /sync/stores/{store_id}/artifact` on the `read_config` scope (Amendment 1 — `/internal` is denied to anything off the box, so ADR-0054's pinned path was unreachable by its only caller); `bootstrap.sh` mints the Garage layout, bucket and key on every deploy, so nothing here needs a human; `POST /admin/releases` uploads the pair and `PUT /admin/config/ota` now refuses a `target_version` the cloud does not host. Amendment 2 records the two mismatches the upload half exposed: **the signature has to cover the bytes `apply` installs** (the workflow signed only the tarball, and unpacking it server-side would install unsigned bytes — it now signs the bare binary too), and **a release has one name, not three** (bare, as `target_version` and the binary's own version spell it). Remaining: nothing on the cloud side. The edge still fetches from the old path — that is R5's.
- **R3** — Per-store installer on the Handoff screen. **Done.** Not `pos-edge install --store <id>` as sketched: the wizard already holds the store id, the cloud URL and the key, so the artifact is a generated `install-pos-edge.sh` carrying `config.toml` and `env` as quoted heredocs — a technician runs one command instead of following a README at 7am in a restaurant. The reason it is worth generating rather than typing is the **slot layout**: since [ADR-0055](adr/0055-edge-ota-updater.md) Amendment 1 the unit starts `bin/current`, and a box with the binary only at `/usr/local/bin/pos-edge` trades perfectly well and silently never self-updates. A re-run refreshes the config, the unit and the rescue copy but **leaves an installed binary alone** — without that guard, re-running on a box that had updated itself over the air would repoint `current` at whatever binary the technician was holding, a silent downgrade of a live shop. It contains the store's key, which the screen and the script both say in as many words.
  **Windows now gets the same thing** (issue #182): a generated `install-pos-edge.ps1` beside the `.sh`, plus a
  parameterised `deploy/edge/install-pos-edge.ps1` for a box the console did not create, both emitted from one
  generator and diff-checked so they cannot drift. The issue attributed the gap to having no way to *check* a
  `.ps1`; the sharper finding was that there was no way to check the `.sh` either — `sh -n` appears nowhere in
  the tree — so the artifacts moved into `dashboard/src/installers.mjs` and a gate now renders them with hostile
  values and parses both languages, `sh -n` in `pnpm build` and PowerShell's parser on the `windows-2022` job.
  Landing it fixed two silent Windows failures: the service was registered on the binary rather than on
  `bin\current`, so a Windows box never self-updated, and nothing set an absolute `store_path`, so SCM's
  `C:\Windows\System32` working directory put the store database and its update slots under `System32`.
- **E4** — Windows service wrapper. **Done** (`crates/pos-edge/src/service.rs`). Two things were worse than "platform code CI cannot exercise". First, the documented install **could not work**: `sc.exe create` on a console program yields *error 1053*, because SCM gives a starting service about thirty seconds to connect back and report `RUNNING` and a plain program never does — so every Windows store was either under a third-party shim or running in a console window, which nothing restarts after a power cut. Second, over-the-air updates could not have worked even with a shim: an install exits so the manager restarts it, and SCM has no `Restart=always` — it has failure actions, applied only when a service *looks* like it failed, so a store that installed a release and exited zero would have sat dark until somebody drove there. So `serve` now returns a [`ServeOutcome`](../crates/pos-edge/src/server.rs) saying whether a stop was a restart, and the wrapper turns that into a non-zero exit code the documented failure action acts on; an operator's own stop still exits zero and stays stopped. The install seam needed no second copy — `installer.rs` already had both platforms' primitives behind two `cfg` functions. What is still gated is SCM's own behaviour ([`gate-register.md`](gate-register.md) P3). The **generated Windows installer** E4 left open (issue #182) has since landed with R3's Windows half — see R3.
- **E5** — Edge UI consumes the **real** menu/locale/tender from `EdgeSession` (kills the hardcoded `ui/src/lib/menu.ts`; publishing a menu now changes the POS). Also: the UI never computes money — every figure comes from the edge. **Done, and the residuals are closed too — there were five, not the three named here.** The three were the shift opening float, the shift screen's expected-cash figure and one Pay-screen label; auditing them found a fourth that was the only *wrong number* rather than a wrong label (`parseWhole(amount(), "VND")`, out by a factor of 100 on any two-decimal currency), and a fifth in `Takeaway.tsx` — the same defect fixed one file over, written the same way and missed (#183).
- **R4** — Real `UpdateInstaller` for Linux (systemd swap → self-test → rollback); Windows follows E4. **Done, shipped with R5** — an installer with no caller is the same gap again. [ADR-0055](adr/0055-edge-ota-updater.md) Amendment 1 records the two things the ADR assumed and the tree contradicted. **The edge cannot write `/usr/local/bin/pos-edge`, and must not be able to**: the unit runs the store under `ProtectSystem=strict` and `NoNewPrivileges`, so the binary moved to a symlink inside the service's own state directory and the sandbox stayed as strict as it is. And **the self-test that gates `commit` is not the one that decides a rollback** — ADR-0048's highest-precedence rule compares against the version the box is *running*, so a pre-commit verdict can never satisfy it, and a store that failed a build pre-commit would have installed the same build forever. There are now two: a smoke test that execs the staged file, and a boot confirmation whose absence past three attempts reverts the box on its own.

### A·P3 — Prove
- **Q1** — In-process end-to-end acceptance suite (dine-in + takeaway), the v1.0 gate. **Done.**
  `pos_edge::compose` is `serve` minus the socket, and `crates/pos-edge/tests/acceptance.rs` drives it:
  the dine-in flow end to end (pair → sign in → seat → order → fire → bump → check → bill → settle →
  clean), the cash shift, relayed takeaway intake, and both auth gates asserted *on the composed
  router*. The sensitivity is measured rather than claimed: deleting the domain-router merge from
  `serve` fails four of the seven cases while the hand-built `domain_flow.rs` suite still passes — which
  is precisely the blind spot that let seven slices ship unreachable. Writing it also found the ninth:
  **a relayed takeaway order cannot be paid for** (see below).
- **Q2** — i18n-parity gate + dashboard code-split. **Done.** The parity gate
  (`scripts/i18n-parity.mjs`, mirrored into both roots, wired into `pnpm build`) requires every locale
  to match `en`'s key set exactly. Its scope is deliberately one property: a *bad* key is already a
  type error (`t()` takes `keyof typeof en`), but a key missing from `vi.json` type-checks and *works*
  via the English fallback — so it ships silently and an operator reads English mid-shift. Both apps
  are in parity today (1233 and 90 keys), so it is a drift guard, verified by breaking it in both
  directions. The code-split made every guarded screen a `lazy()` chunk: **initial JS 540.57 → 268.15
  kB (−50.4%), gzipped 131.47 → 76.67 kB**, 37 route chunks, and vite's >500 kB warning gone. Also
  closed a smaller thing found on the way: `i18n-lint.mjs` was duplicated into both roots without
  being listed in the `mirrored-files` gate, so it was identical only by luck.
- **Q3** — AIP-193 envelope on `/v1` **and** `/admin` (both returned plain text) + ETag/If-Match.
  Scoping it found the job is four times the size it was booked at: **392 string-bodied error
  sites**, not the 185 first counted — the first census used a single-line pattern and missed every
  multi-line `(StatusCode::X,\n "…")` form — and **31 of those are shared per-domain helpers
  carrying 258 call sites between them**, so the real figure is ~620 error response paths. That
  changed the slicing: convert the *shared responders* first, where thirty one-line edits move 258
  paths, then the inline validation refusals (which are also the ones that will carry field-level
  `details`), then ETag.
  - **Q3a — the envelope and every shared responder. Done.** `api_error` is the single constructor
    and takes no status code: it derives one from the body's own `ErrorStatus` through
    `ErrorStatus::http_code`, so a status line and a body cannot disagree. All 30 helpers plus
    `error_response` now go through it. `error_response` also *had* a latent trap — a catch-all arm
    sent `NOT_FOUND`, `ALREADY_EXISTS`, `PERMISSION_DENIED` and `UNAUTHENTICATED` to `500`, blaming
    the server for the caller's own bad request and inviting a retry that could never succeed. It
    was latent, not live: the function has one caller (`/internal/ingest`) and none of those four is
    reachable from it. Deleting the match closes it for the next caller, and a test walks every
    canonical status through `error_response` and fails if the match returns — verified by
    reinstating it. Two integration cases pin what a body change could have broken silently: a
    refused activation still answers **byte-for-byte identically** whether the code was spent or
    never issued (ADR-0050's no-oracle rule, which a richer body is exactly the thing that could
    undo), and a throttled login keeps its `Retry-After` (the half a JSON body cannot carry).
    Mid-conversion the surface is deliberately mixed, and safe by construction: nothing consumes the
    shape — `cloud-sync-http` maps on HTTP status alone and never parses a body — and the console's
    `failure()` now reads the envelope, `{"violations":[…]}` and raw text alike.
  - **Q3b — done, in five slices.** The ~360 inline validation refusals, by surface: non-`/admin` (`/v1`, `/sync`,
    `/internal`, `/activate`) first, then `/admin`. These carry `details` — the field name and a
    stable reason — which is what lets a console form highlight the offending input instead of
    showing a sentence.
  - **Q3c** — ETag on read, `If-Match` required on PATCH, `412` on mismatch. **Done** ([ADR-0094](adr/0094-console-optimistic-concurrency.md), [ADR-0095](adr/0095-conditional-writes-for-collections.md)): six keyed upserts split into `create_*`/`update_*` so a conditional write has something to be conditional on, and collections that had no single row to version gained one.

  **Q3 is closed.** All three sub-slices landed, including the 22 plain-text `400` sites the census missed on its first pass.
- **Q4** — Store hub + URL context `/t/:tenant/s/:store`. **Done, both halves.** The URL is option A
  (the tenant a path segment, the store a `?store=` query, so a link is shareable and a bookmark
  survives an org switch). The hub itself is six read-only cards on the tenant-scoped index
  ([ADR-0099](adr/0099-store-hub.md)), composed from reads that already existed — no route, no
  projection, no migration, no permission — with Reports moved to `/reports`. Two of the six are
  counts rather than lists, and the ADR records why: the cloud projects neither a live out-of-stock
  set nor a shift roster, and the roster is employee personal data needing a lawful basis rather
  than a card.
- **Q5** — `/admin` becomes a real contract: pagination/`q`/sort on the unbounded lists; `/admin` into OpenAPI + the drift gate; fix the webhook header docs↔code mismatch; implement or drop `pos-api-version`; wire or delete the two dead scopes; rate-limit `/v1/orders` and `/sync`. **Done** ([ADR-0098](adr/0098-paged-admin-reads.md) for the paging vocabulary). The header half was not the rename it looked like: two of the four documented webhook headers described a per-event delivery model that was never built, so `pos-event-id` and `pos-delivery-id` were struck from the table rather than corrected, and `pos-api-version` was dropped because nothing read it. `pos-delivery-id` has since **returned as a real addition** with the semantics the transport supports — it names the delivered *page* — under production-readiness **R6**.
- **Q6** — Integrator docs: webhook quickstart (correct HMAC header), auth guide, API tour. **Done** — [`docs/guides/integrate-with-the-api.md`](guides/integrate-with-the-api.md), including an explicit list of what is deliberately not there.
- **Q7** — UX step budget: a measured action budget for the common tasks, failable in CI (add item ≤2 taps, cash settle ≤3, price change ≤4 clicks). **Done** — `ui/scripts/step-tasks.mjs` declares **fifteen** selling tasks (the sketch said "~12"), `ui/scripts/step-budget.mjs` resolves every declared tap against the source, and `ui/tests/replay.spec.mjs` walks the same taps in a browser and asserts each flow reaches its outcome — which closes the case an analyser cannot see, a required tap nobody declared ([ADR-0109](adr/0109-counting-the-taps-an-operator-makes.md)). `dashboard/scripts/step-budget.mjs` measures five office flows and reports rather than rules; gate register **A4** now waits on a decided console ceiling, not on a harness.

### A·P4 — Operate

> **These `O` ids are A·P4's own.** [`cloud-admin-ux-plan.md`](cloud-admin-ux-plan.md) uses `O1`–`O4` for a
> different set — fleet liveness, alerting, sync/OTA closure, reports — and
> [`production-readiness.md`](production-readiness.md) uses `O1`–`O6` for a third. Always write these with the
> `A·P4` prefix, which is what the two references outside this file already do.

> This section and **A·PF** below carried no status markers at all until 2026-09-04, which is why > "how much is left" was not answerable from this document. Two of the five here were already > delivered under other headings. The rest, and all of A·PF, are open.

- **O1** — Alert delivery webhook. **Done** — the `AlertChannel` abstraction with a webhook channel over the existing TLS sender, plus the evaluator loop that fills it.
- **O2** — TCP `:9100` printer transport + a "test print" button.
- **O3** — Store-side WAL-shipping backup.
- **O4** — JetStream capacity probe. **Done** ([ADR-0087](adr/0087-edge-relay-and-event-publish.md) Amendment 2) — the ceiling is the cloud's `cloud.toml` `[nats] max_messages`/`max_bytes`, reconciled against the live stream on each alert pass, because `ensure_stream` is a create-or-get: until now the ceiling in force was whatever the *first* box that ever connected asked for, and no edge release could move it. The reading that pass takes is what finally gives `AlertKind::JetstreamCapacity` a producer. The edge constants stay, demoted to a first-boot floor.
- **O5** — In-code auth for `/internal/*` (a shared-secret header; defence in depth over the network boundary). **Done** ([ADR-0097](adr/0097-internal-route-authentication.md)) — and it recorded what a fleet-wide secret cannot buy: attributability. The two routes a *store* called moved to `/sync` in R5 instead, where the tenant comes from the scoped key rather than from the body.

### A·PF — Performance & durability (turns principle 4 into gates)
- **PF1** — Latency budget as a CI gate: p99 per operation on a standard weak box (2-core/2GB) — add item <30 ms, fire <40 ms, settle <60 ms, `/ws` fan-out <50 ms (kept from P5), config apply <200 ms. Criterion + in-process HTTP benches; a regressing PR goes red.
- **PF2** — Real CCU load test: one edge under 30 devices + 200 QR sessions + vendor intake at peak; cloud under 500 stores long-polling + NATS ingest. Nightly 8-hour soak with p99 + zero-loss thresholds.
- **PF3** — Resource ceilings: edge RSS <300 MB, idle CPU <5 %, edge-UI bundle <250 kB gzip; SQLite/WAL tuning; dashboard code-split.
- **PF4** — Long-run durability: SNTP `ClockSource` + clock-drift alert; prune synced events (the log must not grow without bound); disk-space guard + early alert.

### A·P5 — Pilot (ops, gated)
- **P1** — WS-F security review + human/hardware gate register. **The register is done**
  ([`gate-register.md`](gate-register.md)), together with the go-live sequence it sits inside
  ([`go-live.md`](go-live.md)): twenty-eight gates across human decision, per-store provisioning,
  privacy/legal, real hardware and external registration — plus three chores it first listed as gates
  and does not, which moved to a backlog once an owner asked why so much of a deploy was manual. **The pre-production security review is not** — [`security-review-ws-d.md`](security-review-ws-d.md)
  covers WS-D's ops hardening, not this.
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
- **B6.1** — `/ws` read-only scope + event-type filter, so a third-party KDS plugs in <50 ms on the LAN. **The authentication half moved earlier to S0c and is done** — an unauthenticated socket is a live hole, not an integration feature, so it did not wait for this wave. What remains here is the *scope*: a consumer whose token buys read-only access to a chosen subset of event types, rather than the whole fan-out every paired device now gets.
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

## Program C — Edge Anywhere (Phase 0 recorded; nothing implemented)

`pos_edge` stays the single unit of a store — one process, one SQLite database, one lease, one API
key — and gains exactly one new degree of freedom: **where it runs**. Three `edge_placement` modes
share every line of domain code: `IN_STORE` as today, `HOSTED_BY_OPERATOR` on the operator's own VPS,
and `HOSTED_BY_PLATFORM`, which an admin stands up by choosing a region and pressing Start.
Target shape: 500+ stores, 10+ brands, ~5 countries.

**Phase 0 is done and is the whole of this programme so far: five records, no code.**

- **C0.1** — [ADR-0110](adr/0110-edge-placement-is-a-deployment-axis.md): `edge_placement` is an
  attribute of a store; ADR-0001's offline guarantee is a property of the `IN_STORE` mode only; the
  [ADR-0049](adr/0049-single-active-lease.md)/[ADR-0108](adr/0108-the-lease-generation-is-authority.md)
  lease is the sole authority over which placement is active, and the column is written inside the
  bump's own transaction so the two can never disagree. **Done.**
- **C0.2** — [ADR-0111](adr/0111-a-second-origin-may-address-the-edge.md): a CORS allow-list published
  as an `origins` config node, a configurable base URL defaulting to same-origin, the device token in
  the OS keychain, an `https` pairing URL for hosted placements, a version handshake. **Done.**
- **C0.3** — [ADR-0112](adr/0112-print-agents.md): a paired device may own a printer's transport; the
  edge still renders every byte; a durable per-agent queue with ACK, an idempotent job id and one-hop
  failover along `backup_station_id`. **Done.**
- **C0.4** — [ADR-0113](adr/0113-the-host-agent.md): a dial-out host agent that long-polls
  region-scoped jobs and runs one container per store, so the cloud never dials in. **Done.**
- **C0.5** — [ADR-0114](adr/0114-region-is-required-recorded-visible.md): the hosting region is
  required, recorded and visible; mechanism, never policy. **Done.**

**Two spikes gate every estimate below Phase 0, and neither has been run:** Tauri v2 on Android
against the real `ui/dist`, and Android as an ESC/POS print agent over Bluetooth or USB-host. Until
the second passes, the print agent is the Windows terminal and ADR-0112 stays correct either way.

**Shipped ahead of Phase 1, because it needed no record to land and every hosted placement needs
it:** drain-before-stop. `EventPublisher::run` returned on the stop signal without a last pass, and
`server.rs` dropped the `JoinHandle` the loop was spawned on — so even a loop that drained was
abandoned the moment `serve_until` returned. A stop now runs one bounded drain and something waits
for it (`docs/production-readiness.md` **D8**).

**Phase 1 has begun.** `edge_placement` is a column on `store_lease`, an `EdgePlacement` wire enum in
`pos-proto`, and an optional field on the lease bump — written inside the bump's own statement and by
nothing else, so the record and the lease cannot disagree (**C1**, ADR-0110). It now reaches its
readers: the fleet API carries it, the Fleet console badges it beside liveness, and the alert engine
scores a quiet store by it — a hosted edge gone silent is `Critical`, an in-store one stays
`Warning`, because only one of them means the store has stopped selling.

**Deferred with reasons, not skipped:** `/admin/stores` and `/admin/stores/{store_id}` do not carry
it yet. The single-store route is `PATCH`-only (there is no `GET`), the registry router holds no
handle on the lease, and ADR-0114 already specifies that the two single-store routes gain a
`ConfigTreeStore` handle as part of *its* work — so building it now means building it twice.

`superseded_generation` is a column now (#203): the bump records the generation it displaced, and a
heartbeat reporting *that* generation with an empty outbox clears it — both facts from one message,
because the liveness row COALESCEs depth and generation independently and can hold a pair from two
different beats.

**Three decisions the owner settled on 2026-09-06**, each of which had two accepted records
disagreeing:

- The bump **does** carry `If-Match`. ADR-0110 already said so; the route did not. The deciding case
  is two admins bumping at once with different placements — the single statement serialises them, so
  both get a success and the second placement wins, leaving the first admin believing they moved a
  store somewhere it is not.
- `edge_placement` stays on **`store_lease`**, not the store registry, because that table's only
  write is the bump — structural rather than conventional. ADR-0110's Consequences amended.
- `retired` **gets storage** (`retired_at`, `retired_by`), because `AuditRecorder` is best-effort by
  contract and a trail allowed to drop an entry cannot be the durable record of a decision.

**The three decisions above are built** (#204, #205), and with them the handover is closed end to
end:

- The bump is a **conditional write**. `If-Match` on the row's generation answers `412`; an
  undrained `superseded_generation` answers `422` and takes an explicit `acknowledge_undrained`
  naming the abandoned generation. Both conditions live inside the write statement, never in the
  handler — a read-then-write check is a TOCTOU whose failure mode overwrites the record that a
  displaced machine never drained, using the column added to hold that record.
- A **stopping store beats once more, after its drain**. The automatic clear waits on a heartbeat
  carrying the superseded generation with an empty outbox, and no store had ever sent one: the
  heartbeat loop ended at the stop signal and D8's drain runs after it, so the last thing a cleanly
  stopping machine said was the tick reporting the backlog it was about to clear. Every handover, however
  well it went, needed a person to close it.
- **`retired_at`/`retired_by`** (migration `0054`) plus two audited `/admin` writes: `…/lease/settle`
  (a person attests a powered-off machine is empty, naming the generation they checked) and
  `…/lease/retire` (refused while a handover is in flight, and refused twice rather than overwriting
  the first decision). A bump clears both columns in the same `SET` clause, because they describe the
  *current* handover.
- The **`taking-over`/`settled`/`retired` states** are derived on the fleet row and rendered by the
  console, which offers each act only from the state it is reachable from. `settled` requires the
  generation and the empty outbox to come from *one* heartbeat, because the liveness row COALESCEs
  them independently. A store on generation `0` reports **no state at all** — `0` is a first lease,
  which supersedes nobody, and ADR-0110's wording read literally would have badged a fleet that has
  handed nothing over.

- The **origin allow-list** ([ADR-0111](adr/0111-a-second-origin-may-address-the-edge.md)): an
  `origins` config node of up to eight entries, authored at `PUT /admin/config/origins` and on the
  Channels & payments screen, validated by one rule the cloud and the edge both call, and applied by
  the edge to every route a till uses. `/ws` carries its own `Origin` check rather than a CORS layer,
  because a browser applies no same-origin policy to a WebSocket handshake. A store that publishes
  nothing behaves exactly as it did: the origin that served the page is compared against the request's
  own `Host`, not against the list. ADR-0111's remaining pieces — the base-URL default, the token in
  the OS keychain, the second pairing-URL form, the version response header and the additive-route
  snapshot — are not in it.

**Not started:** the edge container image, the print queue and the host agent — and the two spikes
above still gate every estimate on them.

## Debates settled

The full debate log (D1–D25) lives in the planning artifact. The load-bearing ones:

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
- **D23** — The edge reaches NATS **directly over TCP**, not through the HTTP reverse proxy. Both were viable and performance-indistinguishable at this workload — the publisher drains the outbox in batches every 5 s, ~135 events ≈ 40–100 KB, so ~20 KB/s per store, nowhere near any transport's limit, and WebSocket's per-frame mask is a rounding error there. Direct TCP wins on four things a proxy cannot give: TLS terminates *at NATS*, which is the only way to authenticate a store by **client certificate** (through a proxy the identity dies at the proxy — a header NATS will not read); NATS's own cluster gossip lets clients failover across nodes, which a proxy hides behind internal addresses; the proxy is not a shared chokepoint for both the console and the bus (Caddy runs on `cpus: 0.25` / `mem_limit: 96m` and already carries the long-polls); and there is no upgrade round-trip per reconnect, which mattered for the then-deferred relay live mode (ADR-0062 has since declined that mode, so this is now the weakest of the four). It also needs **no** proxy plugin: the `nats` container publishes `4222` itself and Caddy is not in the path. **Revisit if** the fleet outgrows one NATS node, per-store mTLS is dropped, the bus starts carrying large payloads (the deferred log tail), or the proxy measurably stops being the constraint — the transport is a URL, so reversing costs a config line, not code (`async_nats::connect` takes either scheme from the same binary).
- **D24** — TLS termination is a **fork-level posture, not a fixed choice**. Four are legitimate and a framework must serve all four: ACME HTTP-01 (the default; sslip.io and any A-record domain), ACME DNS-01 (a Cloudflare-managed domain, grey-clouded), **bring-your-own certificate files** (a company with a wildcard or an internal CA and no ACME), and **termination upstream** (a company whose own load balancer, ingress or tunnel already does TLS and where the bundled proxy should do none). An explicit `TLS_MODE` selects a committed per-mode file; nothing overwrites one. **Recorded in [ADR-0090](adr/0090-tls-postures.md)**, which also establishes `secrets/tls/` as the single certificate path — the dependency ADR-0089 was waiting on. The `external` mode is the one with a hidden edge: the app must then trust `X-Forwarded-For`/`X-Forwarded-Proto` from *that* balancer, or the login rate limit collapses every user onto one source IP and one wrong password locks the whole company out.
- **D25** — For mTLS on the bus, the **server certificate and the client CA are different trust decisions**. The server certificate may be public (ACME or brought). The CA that verifies *store* certificates must be **private and ours** — configure a public CA there and anyone who can obtain a certificate from it can speak to the bus, which is the most common way mTLS is misconfigured into a no-op. Store certificates carry the `store_id` as their subject and NATS maps it (`verify_and_map`), which is what finally gives a box a real fleet identity rather than one derived from its store id. Custody follows D1's reasoning: a CA key on the VPS means owning the VPS is owning every store's identity, so the pilot may generate it on the box (documented as a pilot posture) and a fleet moves it offline before scale.
- **D26** — Where a store's edge runs is a **deployment axis, not a fork**: one binary, one lease, three `edge_placement` modes. Three things follow and are recorded rather than assumed. Offline trading belongs to `IN_STORE` alone, so the console must show a store's mode wherever it shows its health — a hosted store with no internet cannot sell, and saying so plainly beats a mode that silently degrades. A device-local write buffer is **declined outright**: receipt numbers, stock and shift totals all assume one authority, and a second writer turns every conflict into a money question the framework already solved at the store level. And the word `placement` was already taken twice (`MenuPlacement`; the OTA rollout placement at `/admin/config/ota/placement`), so the newcomer disambiguates itself as `edge_placement` rather than renaming a published route. **Recorded in [ADR-0110](adr/0110-edge-placement-is-a-deployment-axis.md)**; ADR-0111 to ADR-0114 build on it.

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
