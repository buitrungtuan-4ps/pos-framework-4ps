# Roadmap v4 — The POS becomes a customization platform

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-11-10
**Supersedes the ordering of** [roadmap-v3.md](roadmap-v3.md), whose open items are folded in below

Approved by the owner on 2026-11-03, together with the thirteen decisions listed under
[Decisions](#decisions). Roadmap v3 stays as the record of what it delivered.

## Goal

HQ designs screens, flows, rules and data in the console, pushes them to stores in about one second,
and every tap on a till paints within 10 ms. Money, tax and receipts stay in the core. The pilot in
Vietnam starts only after the overhaul is complete.

## What the owner decided

These are the requirements every phase is checked against.

| Topic | Decision |
|---|---|
| What is customizable | Screens and buttons; flows (required, optional and conditional steps, "do X before Y"); reactions ("when … if … then …"); custom data: new fields on existing objects and new object types with their own screens |
| What stays in the core | Money, tax and receipts, driven by configuration (promotions, fees, tax packs). A flow may only *invoke* an existing money operation; it never computes an amount |
| Technology | Console designers; conditions in CEL (not Turing-complete, no loop macros, length-limited); custom code as WebAssembly functions (pure, typed input to a decision, fuel and memory limits, signed, ring rollout), only when a real case needs it. Not Rhai |
| Who designs | HQ. A store is either locked to the version HQ picks, or chooses among the versions HQ allows. A store never edits |
| Dimensions | Brand, store, device, role, sales channel, time slot |
| Business-type templates | Six: full-service chain restaurant, café and bubble tea, fast food, bar with tabs, bakery and retail (barcode, scale), self-order (QR, kiosk) |
| Speed | At most 10 ms from the till receiving a tap to the next painted frame, p95, release build; the wait for an external device or connection is excluded |
| Money with paint-first | The drawer opens and the receipt prints only after the edge commits; the till pre-checks refusals it can predict; a refusal rolls the screen back with a blocking message |
| Distribution | An active push in about one second (plus the network) over a connection the store opens; the 30 s poll stays as the fallback |
| External calls | Only through the cloud, as a gateway; a till never calls the internet |
| Devices | All of them, in this order: Windows, tablet, Android, iOS, Linux. The reference box is an Intel or ASUS NUC mini PC, on any Windows edition |
| Countries | Vietnam, Japan and India complete in code and tests; the pilot is in Vietnam; Japan and India go live when their legal registration is done |
| Personal data | Every custom field declares whether it is personal data. If it is, it is stored apart, never in events or logs, with a retention period and a consent step |
| Order | The overhaul first, then the pilot |
| No reference NUC yet | Measure by CPU-throttled emulation in CI and by telemetry from real tills; real devices before phase 9 |

## Phases

Two lanes run side by side, with at most two agents at a time. Lane A is the cloud and the console;
lane B is the edge and the till.

| Phase | Lane | Scope | Exit criterion |
|---|---|---|---|
| **0 — Foundations** (2–3 weeks) — **done** 2026-11-10 ([exit record](metrics/package-push-2026-11-phase-1-exit.md#phase-0-milestone-1-is-met); the Tauri mobile spike built nothing and moves to phase 8) | both | The decisions below; a tap-to-paint baseline per act at 1× and 4× CPU throttle; spikes: CEL in the till (WebAssembly) and on the edge, a screen rendered from a package, a NATS wake, Tauri v2 on Android and iPad; unit tests for `ui/` (gate A5) | **Milestone 1:** the decisions are merged as Accepted and the baselines are recorded under `docs/metrics/` |
| **1 — Package and distribution** — **done** 2026-11-10 ([exit proof](metrics/package-push-2026-11-phase-1-exit.md): console write to running p95 118 ms; B7.2 and B7.5 still partial, B7.3 completed after it by per-device drift on the fleet) | A | Package versions, lock or choose, rollback, minimum edge version, the six dimensions ([ADR-0189](adr/0189-a-store-runs-a-package-version-hq-pins.md)); the push in about one second ([ADR-0195](adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)); roadmap-v3 B7.2, B7.3 and B7.5 | A package version published in the console is running on a test edge within one second, a store can be rolled back to the previous version, and an edge too old for a version keeps the one it runs |
| **2 — Runtime** — **done** 2026-11-06 ([exit proof](metrics/till-latency-2026-11-phase-2-exit.md); #1180–#1203) | B | Typed action catalogue, screen renderer, flow engine on the till with a re-check on the edge, CEL rules, local reactions, paint-first for every act with the three money safeguards; today's screens become the **default package**; guest count as a flow step; the 10 ms gate ([ADR-0197](adr/0197-every-tap-paints-within-ten-milliseconds.md)) | The default package renders the till with no visible change, and every act it contains passes the 10 ms gate |
| **3 — Custom data** — **done** 2026-11-07 ([exit proof](metrics/custom-data-2026-11-phase-3-exit.md); #1165–#1178, #1205–#1218) | B | Custom fields, lists and object types; personal-data classification; sync, reports, printing; a receipt template per store and channel | A custom object type is designed, synced, entered on a till, reported and printed, and a field marked personal never reaches an event or a log |
| **4 — Console designers** — **done** 2026-11-12 ([exit record](metrics/console-designers-2026-11-phase-4-exit.md); #1221–#1257; roles on a store and reports by area left open) | A | Drag-and-drop screens, a flow graph, a rule builder that writes CEL, a data designer, live preview through the till's own renderer, validation before publish, version management; the multi-floor drag-and-drop floor plan | HQ builds and publishes a package without writing JSON or CEL by hand, and the preview matches the till |
| **5 — Gateway and functions** — **done** 2026-11-14 ([exit record](metrics/gateway-functions-2026-11-phase-5-exit.md); #1261–#1289; functions deferred by ADR-0216, busy answers and a durable answer queue left open) | A | Every external call through the cloud; WebAssembly functions; webhook filters; delivery-vendor intake | A till's external lookup and a delivery order both go through the gateway, and a partner places an order through the public API (functions deferred by [ADR-0216](adr/0216-integrations-come-in-three-tiers-and-functions-wait.md)) |
| **6 — Templates** | B | The six business-type templates, and what they need, each with its own ADR: bar tabs, weighed items, self-order, barcode and PLU, the pre-bill event, comp and price override, partial payments of a split bill visible to other tills | Each template sells end to end in the simulator for each of the three countries |
| **7 — Countries** | A | The Vietnamese e-invoice adapter and receipt series; Japan; India | Each country's legal acceptance suite passes |
| **8 — Devices** | B | Kiosk behaviour on any Windows edition on the NUC; Tauri mobile on Android and iOS; a card terminal; the edge container image and its host agent | The till runs on each device class in the priority order, and a card payment completes through the terminal port |
| **9 — Hardening and pilot readiness** (3–5 weeks) | both | The roadmap-v3 items still open; a security review of every new surface; real-hardware gates; 10 ms on real devices | **Milestone 2:** the owner's go or no-go for the pilot |

## Decisions

Each is an ADR, accepted with this plan. The numbers were reserved together.

| ADR | Decision | Amends or extends |
|---|---|---|
| [0189](adr/0189-a-store-runs-a-package-version-hq-pins.md) | A customization package: a versioned bundle of screens, flows, rules and data; a store locked to a version or choosing among allowed ones; six-dimension resolution; a minimum edge version; last-known-good | ADR-0033, ADR-0066, ADR-0160 |
| [0190](adr/0190-a-screen-is-drawn-from-the-package.md) | Screens from data: a component catalogue, device classes, a fallback for unknown components, labels through translations, the tap budget checked at publish | ADR-0109 |
| [0191](adr/0191-every-act-is-in-a-typed-catalogue.md) | An action catalogue: every act typed, with its permission and its effect class (UI only, local edge write, money through a core command, external through the cloud) | — |
| [0192](adr/0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md) | A flow engine: a graph of required, optional and conditional steps with prerequisites; the till runs it and the edge re-checks it; a step in progress keeps the rules it started with | — |
| [0193](adr/0193-conditions-and-reactions-are-written-in-cel.md) | Rules in CEL: one evaluator on the edge and in the till, no loop macros, length limits; "when … if … then …" reactions | a new dependency |
| [0194](adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md) | Custom data: fields, lists and object types; storage and sync; additive events; personal data stored apart with a retention period; additive schema changes only | relates to ADR-0076 |
| [0195](adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md) | The cloud wakes a store over the NATS connection the store opens, and the store pulls; the 30 s poll stays as the fallback | ADR-0031, ADR-0039 |
| [0196](adr/0196-every-act-paints-first-and-money-waits-for-the-edge.md) | Paint first, write after: every act paints at once, with three money safeguards | ADR-0173 decision 4, ADR-0188 |
| [0197](adr/0197-every-tap-paints-within-ten-milliseconds.md) | The 10 ms budget: its definition, its measurement (CI emulation and till telemetry) and its gate | replaces roadmap-v3 PF1's till targets |
| [0198](adr/0198-a-store-reaches-the-internet-only-through-the-cloud.md) | The cloud as gateway: secrets in the cloud; retries, idempotency and timeouts; private address ranges blocked | extends ADR-0153 |
| [0199](adr/0199-a-function-is-signed-pure-webassembly-that-returns-a-decision.md) | WebAssembly functions: pure functions returning decisions, fuel and memory limits, signed, rolled out by ring, run by `wasmtime` on the edge | amends ADR-0083 |
| [0200](adr/0200-six-business-type-templates-ship-with-the-framework.md) | Six business-type templates, shipped with the framework like country packs, tested as a template-by-country matrix | — |
| [0201](adr/0201-the-till-runs-on-every-device-class.md) | Devices: Tauri v2 mobile for Android and iOS, kiosk behaviour that does not depend on the Windows edition, the device class as a screen dimension | extends ADR-0147 |

## Where roadmap v3's open items went

| Roadmap v3 | Now |
|---|---|
| D5 (live server-to-edge push deferred) | Replaced by ADR-0195, phase 1 |
| B7.2 lock by path, B7.3 per-device drift, B7.5 diagnostics on the edge | Phase 1 |
| B7.4 settings per layer | Phase 4, with version management |
| PF1 latency targets for the till | Replaced by ADR-0197, phase 2; the edge's own hot-path budgets stay |
| B8.4 covers on seating | Phase 2, as the guest-count flow step |
| B3.2 the `receipt` node per store and channel | Phase 3 |
| B8.2 and B8.3 the drag-and-drop floor plan | Phase 4 |
| B6.2 webhook filters, B6.3 and B6.4 delivery-vendor intake, B9.3 the connector framework | Phase 5 |
| B2.1 the pre-bill event, B2.3 comp and price override, B2.4 split-bill payments, B5.1 barcode and PLU, B5.2 quick sale, tabs | Phase 6, each with its own ADR |
| B4 what is left of it, B10.1, B10.2, the Vietnamese e-invoice | Phase 7 |
| B9.2 and B10.3 the card terminal, Program C's container image and host agent, the Android spike of ADR-0147 | Phase 8 (the spike in phase 0) |
| Everything else still open: A·P4 O2 and O3, PF2 to PF4, A·P5, B6.1, B6.5, the rest of B2 and B3, B9.4, B10.4 | Phase 9 |

## Open items the phases raised

| Item | Phase |
|---|---|
| A store's own people choose their package version in the console. `admin.package.choose` is in the store permission catalogue ([ADR-0189](adr/0189-a-store-runs-a-package-version-hq-pins.md)). Nobody at a store signs in to the console, so until an ADR decides store-scoped console sign-in, HQ chooses for a store through `console.config.publish` | Phase 4 |

## Gates

- **Milestone 1**, after phase 0: the thirteen decisions are Accepted and the baselines are measured.
- **Milestone 2**, after phase 9: the owner decides whether the pilot goes ahead.
- **Every change** runs `just preflight` and the gates `AGENTS.md` names. A new port, a `pos-proto`
  change, a new dependency or an event change waits for its ADR to merge.
- **The non-negotiables stay:** the store sells offline; the store only dials out; an event and its
  transaction commit together; money is an integer; country law lives in country crates; no personal
  data in events or logs; changes are additive.

## How the work runs

- The owner approved the whole plan once. Agents implement it and merge it piece by piece, each piece
  a reviewed pull request that a human merges.
- Progress notes stay short. A full report follows each phase.
- A decision the plan does not cover stops the work and goes back to the owner as a new ADR.
