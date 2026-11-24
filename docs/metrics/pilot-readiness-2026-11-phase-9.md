# Pilot readiness at the end of phase 9, November 2026

**Status** Awaiting the owner's go or no-go · **Owner** @maintainers-architecture · **Measured** 2026-11-24

This is the record for Milestone 2 of [roadmap v4](../roadmap-v4.md): the evidence the owner needs
to decide whether the Vietnam pilot goes ahead. It is a go/no-go checklist. Each item is
**green** (done, with evidence), **amber** (done in code, with a decision, a confirmation or a
measurement still owed) or **red** (not done, and it blocks a pilot that takes real money). Like the
phase exit records, it records one moment and is not regenerated.

**Measured at** `9ecb6414` on `main`. Phases 7 and 8 ran at the same time as phase 9, and both had
merged their exit records by then.

> **This page does not decide.** Milestone 2 is the owner's decision. The recommendation in §4 is
> **AI-generated analysis** written for that decision. It is not a decision, and it is not expert
> human opinion. Where a legal requirement is named, it is to be confirmed by legal counsel or the
> accountant.

## 1. The checklist

### The overhaul's phases

| # | Item | State | Evidence |
|---|---|---|---|
| 1 | Phases 0 and 1: the decisions, the baselines, and a package pushed to a store in about a second | Green | [package-push-2026-11-phase-1-exit.md](package-push-2026-11-phase-1-exit.md): console write to running, p95 118 ms |
| 2 | Phase 2: the runtime, the default package, and the 10 ms gate in CI | Green, in emulation | [till-latency-2026-11-phase-2-exit.md](till-latency-2026-11-phase-2-exit.md): every act's p50 is under 10 ms at a 2× CPU throttle, gated on every pull request. Real devices are item 20 |
| 3 | Phase 3: custom data, with personal fields kept apart | Green | [custom-data-2026-11-phase-3-exit.md](custom-data-2026-11-phase-3-exit.md) |
| 4 | Phase 4: the console designers | Green | [console-designers-2026-11-phase-4-exit.md](console-designers-2026-11-phase-4-exit.md) |
| 5 | Phase 5: the gateway, webhooks, delivery intake and the public API | Green | [gateway-functions-2026-11-phase-5-exit.md](gateway-functions-2026-11-phase-5-exit.md). Functions are deferred by ADR-0216 |
| 6 | Phase 6: six templates, each selling in every country in the simulator | Green | [templates-2026-11-phase-6-exit.md](templates-2026-11-phase-6-exit.md): 24 of 24 matrix cells |
| 7 | Phase 7: countries. Vietnam's e-invoice model, series, schedule, submission queue and acceptance suite; Japan's rounding; India's GST facts | Amber: met for Vietnam and Japan, partial for India | [countries-2026-11-phase-7-exit.md](countries-2026-11-phase-7-exit.md) (#1362); #1340 to #1342, #1345, #1348 to #1350, #1355, #1359 to #1361. India's IRP, UPI confirmation, GSTR-1, the HSN/SAC field and IGST are not built, and how deep India goes is an owner decision. India is not in the pilot |
| 8 | Phase 8: devices. A card through the terminal port, the edge container image, POS Station's kiosk mode | Amber: the card criterion is met against a fake terminal; device classes on hardware are not | #1339, #1354, #1356, #1357, #1358 (ADR-0231, Proposed). Exit record: [devices-2026-11-phase-8-exit.md](devices-2026-11-phase-8-exit.md) (#1366). The Windows kiosk is built and not yet run on a NUC; the tablet runs in a browser only; Android and iOS are not built and wait on the owner's choice in ADR-0231; Linux runs in CI; the host agent `pos_host` is not built |

### Security

| # | Item | State | Evidence |
|---|---|---|---|
| 10 | A security review of every surface phases 1 to 6 added | Green | [security-review-overhaul.md](../security-review-overhaul.md) (#1346): no Critical or High finding |
| 11 | Low findings fixed in phase 9 | Green | G5 (#1343), K1 (#1347), G4 (#1352), K3 (#1351) and K4 (#1353). The rest are tracked in the review |
| 12 | Dependency advisory `url` RUSTSEC-2026-0592 | Green | D1, fixed by #1344; `cargo deny` runs on every pull request |
| 13 | **G1: the guest page shares the console's origin** | Amber | A deployment decision: serve `/qr/` and `/v1/qr/*` on a host of their own before guests scan a code |
| 14 | **K2: a kiosk's device token lives in a public browser** | Amber | Waits for phase 8's kiosk lockdown and hardware gate P13 |
| 15 | The surfaces phases 7 and 8 added: the e-invoice submission queue and the HQ symbol route (#1350, #1360), the card terminal route and its park and resolve (#1354), the container image (#1356), POS Station's kiosk mode (#1357) | Amber | Not covered by the review, which was made on phases 1 to 6, and merged after it. Each needs a section in [security-review-overhaul.md](../security-review-overhaul.md) and a place in the pentest's scope |
| 16 | **An independent pentest** (gate L5) | Red | Not commissioned. Scope: [security-review.md](../security-review.md) §9 and [security-review-overhaul.md](../security-review-overhaul.md) §12 |

### Hardware and speed

| # | Item | State | Evidence |
|---|---|---|---|
| 20 | **10 ms on the reference NUC** (P14) | Red, awaiting hardware | No NUC yet. The runbook is ready: [real-hardware-gates.md](../real-hardware-gates.md) |
| 21 | **10 ms on the tablet and phones** (P15) | Red | No devices, and the till's tap telemetry (ADR-0197 decision 5) is not built, so even with devices it cannot be measured on Android or iOS |
| 22 | The kiosk lockdown (P13), resource ceilings (P16), one edge at peak (P17) | Red, awaiting hardware | [real-hardware-gates.md](../real-hardware-gates.md). P17 also needs a load driver that is not written |
| 23 | The earlier hardware gates P1 to P12: the bus port, the keyring across a reboot, the Windows service, an OTA rollback, power loss, the 222 events/s soak, printers, KDS | Red, awaiting hardware | [gate-register.md](../gate-register.md) §6 and [go-live.md](../go-live.md) "Pilot checklist" |
| 23a | Phase 8's own device gates P20 to P23 ([devices-2026-11-phase-8-exit.md](devices-2026-11-phase-8-exit.md)) and the purchases they need: a NUC, an Android tablet and phone, an iPad, a card terminal and a scale | Red, awaiting hardware and an owner purchase | [gate-register.md](../gate-register.md) §6. Also not built: a crash watchdog for POS Station and a native PIN exit |
| 23b | **The mobile till's token store** ([ADR-0231](../adr/0231-a-phone-keeps-its-till-token-in-a-store-the-owner-chooses.md), Proposed) | Amber, owner decision | Android and iOS are not built until the owner chooses |
| 23c | The host agent `pos_host` | Amber | Not built. It blocks only a pilot hosted on the platform, not an in-store one |
| 24 | **PF3 bundle budget** | Amber, owner decision | The till's JavaScript and CSS are 162 kB gzipped, under PF3's 250 kB. The CEL evaluator's WebAssembly adds 294 kB, so the total is 456 kB. [ADR-0202](../adr/0202-cel-as-the-crate-and-the-spike-found-it.md) left this budget open. The choice is to accept 456 kB (it loads at boot, never on a tap), ship the `opt-level=z` build (237 kB, evaluation 1.5 to 2 times slower) or raise PF3's figure. Measured on 2026-11-24 with `pnpm build` in `ui/`, gzip level 9 |

### Legal, privacy and people

| # | Item | State | Evidence |
|---|---|---|---|
| 30 | **The PDPD lawful basis, the DPIA, retention and consent for Vietnamese stores** (L1, L2, L6) | Red | Owed by legal and the DPO before the first live store. The DPIA notes for a delivery recipient and a tab label are in [data-residency.md](../data-residency.md) |
| 31 | Where Vietnam's data is hosted | Green, decided | The owner decided on 2026-10-14 that Vietnamese stores' cloud is hosted in Vietnam (L6). The pilot's cell must be deployed there |
| 32 | Where Japan's and India's data is hosted, and the processors abroad (B10.4) | Amber, owner decision | Not needed for the Vietnam pilot. Set out in [data-residency.md](../data-residency.md#b104-the-decision-the-owner-owes) |
| 33 | **The Vietnamese e-invoice provider** | Red, owner decision | No vendor is chosen. Phase 7 built against the port with a sandbox provider and contract tests, so **nothing reaches the tax authority** until a real provider is chosen, contracted and built. Also not built: feeding the submitter from `fiscal.invoice.issued`, its durable state and an overdue alert; a console invoice list and corrections; the till's buyer form; the invoice number on the receipt ([countries-2026-11-phase-7-exit.md](countries-2026-11-phase-7-exit.md)) |
| 34 | When the e-invoice is issued | Amber | A published schedule with five modes; the Vietnamese default is `IMMEDIATE` ([ADR-0229](../adr/0229-the-e-invoice-issue-moment-is-a-schedule.md), the owner's decision). The legal limit and the buyer record's retention are to be confirmed by legal or the accountant |
| 34a | **Each Vietnamese store's e-invoice symbol is set by hand** | Red, action before each store sells | Since #1355 a Vietnamese store with no symbol published refuses to settle (`409 E_INVOICE_SYMBOL_MISSING`), by design ([ADR-0225](../adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md), [ADR-0229](../adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)). **Action: HQ publishes the symbol for each pilot store** through `PUT /admin/config/einvoice-symbol` (#1360). There is no console screen and no automatic allocation, and a replacement PC needs a new symbol, so replacing a store's box also needs this step. The symbol letters and Vietnam's issue limit (by the end of the sale's day) are still to be confirmed by legal or the accountant |
| 35 | **A real card terminal** | Red, owner decision | A card is taken through the terminal port, with an unknown result parking the bill ([ADR-0230](../adr/0230-a-card-is-taken-through-the-terminal-port-and-an-unknown-result-parks-the-bill.md), #1354), proved against a fake terminal only. The terminal vendor for Vietnam (roadmap-v3 B10.3) is not chosen. Until one is, a card is taken on a standalone terminal and recorded by hand. Also not built: a console list of parked or unknown card payments |
| 36 | Signing keys and maintainers (H1 to H3, H16, H17) | Red | No record in the repository that they are done. H16 and H17 were deferred by the owner on 2026-10-14 |
| 37 | The drawer notice wording (L7) and full-disk encryption where personal values are kept (L8) | Red | Legal and the DPO, and the store's IT |

### Roadmap v3's open items

Roadmap v4 sent these to phase 9. Each is triaged as **done** (found delivered), **done now** (small
and safe, delivered in phase 9) or **deferred**, with the reason.

| Item | Triage | Evidence or reason |
|---|---|---|
| A·P4 O2, the TCP `:9100` printer and a test print | Done | `crates/adapters/printer-escpos/src/tcp.rs`; `POST /api/printers/{id}/test`, which the till's printer list calls. Marked done in [roadmap-v3.md](../roadmap-v3.md) (#1346) |
| A·P4 O3, store-side WAL shipping | Deferred, half answered | [ADR-0124](../adr/0124-a-store-that-can-be-restored.md) archives the whole store database off the box on a schedule, so the recovery point is one interval. Continuous shipping waits on the A4 spike. The pilot runs on the archive, and restoring it is a pilot checklist step (H10) |
| PF2, peak load and an eight-hour soak | Deferred to hardware | Gates P17 (one edge at peak) and P6 (222 events/s). P17's load driver is not written |
| PF3, resource ceilings | Measured in part; the rest deferred to hardware | The bundle is item 24. Memory and idle CPU are gate P16 |
| PF4, the disk-space guard | Deferred | The edge forbids `unsafe`, and no crate in its dependency set reads free space. Admitting one needs a dependency ADR. Pruning and the clock reading are done ([roadmap-v3.md](../roadmap-v3.md) PF4) |
| A·P5 P1, the security review | Done now | [security-review-overhaul.md](../security-review-overhaul.md) |
| A·P5 P2, a real-hardware pilot and soak | Deferred to hardware | Items 20 to 23 |
| B6.1, a read-only `/ws` scope with an event-type filter | Deferred | A token kind for third-party readers is a port and permission change, so it needs an ADR first. No pilot integration needs it: the pilot's KDS is the framework's own |
| B6.5, resuming `/ws` across an edge restart, a snapshot, a contract document | Deferred | It needs a read in commit order that the event-store port does not offer, so it is a port change with an ADR. The in-memory resume (1,024 frames) covers a Wi-Fi drop, which is the pilot's case |
| The rest of B2 | Mostly done | Delivered: the pre-bill (ADR-0218), void with reasons (ADR-0115), comp and price override (ADR-0219), split payments (ADR-0220), split, merge and transfer, and courses with fire rounds. **B2.6 serving style** is not delivered, and is deferred because nothing the pilot sells needs it |
| The rest of B3 | Mostly done | The receipt template per store and channel (ADR-0207) replaced B3.1's document kind. Raster printing of any script (ADR-0102) is B3.3. A station's printer and its backup are bound on the floor (B3.4). Whether the pilot's printers accept the raster is gate P9 |
| B9.4, a plug-and-play proof kit | Deferred | A sample KDS and a sample connector in the e2e suite prove that anyone can integrate, which the pilot does not depend on. The demo connector and `examples/minimal-edge` cover part of it |
| B10.4, residency, legal and a pentest | Owner decision | Items 16, 31 and 32 |

## 2. What blocks the pilot

A pilot that takes real money from real guests in Vietnam is blocked by the red items. Grouped by
who can clear them:

- **The owner decides:** the e-invoice provider (33); a card terminal and acquirer, or the decision
  to pilot with a standalone terminal recorded by hand (35); the device purchases (23a); the mobile
  token store (23b); how deep India goes (7); the pentest firm and date (16);
  the PF3 budget (24); the guest page's own host (13).
- **Legal and the DPO confirm:** the PDPD lawful basis, the DPIA, retention and consent (30); the
  drawer notice (37). Whether Vietnam requires a POS e-invoice for each sale in the pilot's
  setting, and from when, decides how hard item 33 blocks.
- **Hardware is bought and tested:** the reference NUC and the pilot's tablet and phones, then
  gates P1 to P17 and phase 8's P20 to P23 (20 to 23a).
- **Code still to write before the pilot:** the till's tap telemetry (21), the P17 load driver
  (22), and the rest of phase 7's Vietnamese e-invoice work (33), and phase 8's watchdog, PIN exit and parked-payment list (23a, 35).
- **People:** the signing keys' custody and the two named maintainers (36).
- **Each pilot store, before it sells:** its e-invoice symbol set by HQ through
  `PUT /admin/config/einvoice-symbol`, and again after its box is replaced, or the store refuses
  to settle (34a).

## 3. What is ready

The overhaul's phases 1 to 6 each met their exit criterion with a record. The framework sells end to
end in the simulator for every template and country. Its security review found no Critical or High
finding. Each surface fails closed on its credential, and no personal data reaches an event or a log.
In CI, the 10 ms budget is enforced on every pull request in emulation. Vietnam's hosting is decided.

## 4. Recommendation (AI-generated analysis, not a decision)

> Written by an AI agent from the evidence above, for the owner to weigh. It is not a decision and
> not expert human opinion.

**No-go for a pilot that takes real money now. Go for a dry-run pilot as soon as the hardware is in
hand.**

- **Why not a full pilot now.** Five red items cannot be cleared by code: the e-invoice provider,
  the card terminal, the PDPD sign-off, the pentest and every real-hardware gate. Any one of them
  is enough to stop a store that takes real money. Item 33 and item 30 are the ones legal must
  answer first.
- **What a dry run would be.** One store, or a training copy of one, on the reference NUC and the
  pilot's devices, with staff and test guests, no real customers' personal data, cards on a
  standalone terminal, and e-invoices issued only to the sandbox provider (each store still needs
  its symbol set, item 34a). It runs the go-live pilot checklist and gates P1 to P17 and P20 to
  P23, and it measures 10 ms on the real devices. It needs the hardware bought, but none of the
  vendor choices and none of legal's sign-offs, and what it finds would feed the go/no-go.
- **What would turn this into a go.** Every red item in §1 green or accepted in writing by the
  owner, with a record under `docs/metrics/` for each hardware gate and a pentest report with no
  open High or Critical finding.

## 5. Keeping this page honest

This page is a snapshot. The owner's go or no-go, when it is made, is recorded in
[roadmap-v4.md](../roadmap-v4.md) row 9, with the date and anything accepted in writing.
