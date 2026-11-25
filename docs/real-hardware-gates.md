# Real-hardware gates for the pilot, and how to run them

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-11-24
**State** Awaiting hardware: no reference NUC, tablet or phone is available to the project yet
**Relates to** [`gate-register.md`](gate-register.md) §6 (where each gate is a row) ·
[ADR-0197](adr/0197-every-tap-paints-within-ten-milliseconds.md) and
[ADR-0208](adr/0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md) (the 10 ms
budget and its measurement) · [`capacity-and-reliability.md`](capacity-and-reliability.md) ·
[`roadmap-v4.md`](roadmap-v4.md) phase 9

Roadmap v4 phase 9 asks for real-hardware gates and for 10 ms on real devices. Neither can run in
CI or in the cloud containers the agents work in. This page defines each gate: what to measure,
the threshold that passes, the tooling that already exists, and what is missing. A person with the
hardware runs them and records the result under `docs/metrics/`. Until then each gate reads
**awaiting hardware** in the pilot-readiness report.

## The gates

The rows P13 to P17 are new in [`gate-register.md`](gate-register.md) §6. P5 (power loss), P6 (the
222 events/s soak), P7 (printer, KDS and card-terminal soak) and P2 to P4 and P8 to P12 were already
there, and they stay as they are written.

| # | Gate | Pass threshold | Tooling | Missing |
|---|---|---|---|---|
| P13 | **The kiosk cannot be left without a PIN** on the reference NUC, in the Windows kiosk mode phase 8 sets up ([ADR-0201](adr/0201-the-till-runs-on-every-device-class.md)) | With a keyboard and a mouse attached, none of these reaches a desktop, another window, developer tools, browser storage or the address bar: Alt+Tab, Ctrl+Alt+Del (except to sign out, which returns to the kiosk), Win key, F12, Ctrl+Shift+I, a long press or right click, an edge swipe on a touch screen, and unplugging and replugging the network. A power cycle comes back to the kiosk screen on its own | Phase 8's kiosk lockdown | The NUC. Security review finding **K2** depends on it |
| P14 | **10 ms on the reference NUC.** Every act the default package draws, from the tap to the frame ready to paint ([ADR-0208](adr/0208-a-tap-is-measured-to-its-frame-ready-and-gated-per-pull-request.md) decision 1), at the NUC's own speed | For every act: at least 100 samples, **p95 ≤ 10 ms**, p50 ≤ 10 ms, and no long frame (ADR-0208 decision 2), in two runs out of two. A release `minimal-edge` and a production till build | `ui/perf/gate.mjs` and `ui/perf/tap-to-paint.spec.mjs`, run on the NUC itself with `POS_PERF_CPU_THROTTLE=1` | The NUC |
| P15 | **10 ms on the tablet and the phones** the pilot uses: an Android tablet, an Android phone and an iPhone or iPad, each running the till as it ships in phase 8 | The same threshold as P14, per device class | None yet. The harness drives a desktop Chromium over the DevTools protocol, which cannot reach the Tauri webview on Android or iOS. The till's own telemetry ([ADR-0197](adr/0197-every-tap-paints-within-ten-milliseconds.md) decision 5: buckets per act, device class and release, forwarded through `MetricsSink`) is **not built**: nothing in `ui/src` measures frame-ready outside the harness, and the edge has no route that takes the buckets | The devices, **and** the telemetry. Without the telemetry P15 cannot be measured even with the devices in hand |
| P16 | **Resource ceilings on the NUC** (roadmap-v3 PF3): the edge after eight hours of trading, with a till, a KDS and a kiosk attached | Edge resident memory **< 300 MB**; edge CPU **< 5 %** when idle (no tap for ten minutes); the till's JavaScript and CSS **< 250 kB gzip** (this one can be checked in CI without hardware) | Windows Performance Monitor (`Process(pos_edge)\Working Set - Private`, `% Processor Time`) sampled every minute. On Linux, `pidstat -r -u -p $(pidof pos_edge) 60` | The NUC. The bundle check is not in CI yet |
| P17 | **One edge at peak** (roadmap-v3 PF2): 30 devices on `/ws`, 200 guest QR sessions reading the menu and ordering, and delivery orders arriving through intake, for one hour on the NUC | p99 of add-line, fire and settle at most PF1's figures (30, 40 and 60 ms) at the edge; **no lost event** (the outbox and the cloud's ingest agree on the count); no edge restart | `crates/pos-edge/tests/perf_budget.rs` (`just bench`) measures the edge's own budgets in-process. The simulator's stress models (`crates/pos-simulator/src/stress.rs`) model throughput and do not drive a real edge | The guest QR sessions and delivery intake, which reach a store through the cloud: `ui/perf/load-p17.mjs` (`pnpm load:p17`) pairs the devices, holds their `/ws` sockets open and sells counter orders on each, and reports p50, p95 and p99 per act, failures and the outbox depth; run a cloud-side QR and intake load beside it on the NUC |

## How to run P14 on the NUC

The harness spawns its own `minimal-edge` and drives a local Chromium-based browser. Microsoft
Edge, which every Windows edition ships, is Chromium-based and speaks the same DevTools protocol.

1. Install the repository's toolchain (Rust, Node, `pnpm`), as [`ui/README.md`](../ui/README.md)
   describes.
2. Close every other application. Set the power plan to the one the store will use, not "High
   performance" unless the store uses it. Plug the NUC into the display the store will use, and note
   its refresh rate.
3. In a shell at the repository root:

   ```powershell
   cd ui; pnpm install; pnpm build; cd ..
   cargo build -p minimal-edge --release --locked
   cd ui
   $env:POS_EDGE_BIN = "..\target\release\minimal-edge.exe"
   $env:POS_UI_CHROMIUM = "C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
   $env:POS_PERF_CPU_THROTTLE = "1"
   $env:POS_PERF_OUT = "test-results\nuc-run-1.json"
   node perf/gate.mjs
   ```

4. Run it a second time with `POS_PERF_OUT` set to `nuc-run-2.json`.
5. Record both runs in `docs/metrics/till-latency-<yyyy-mm>-nuc.md`. Give the NUC's model, CPU,
   memory, Windows edition and display refresh rate, and for each act n, p50, p95 and long frames,
   in the shape of [`till-latency-2026-11-phase-2-exit.md`](metrics/till-latency-2026-11-phase-2-exit.md).
6. **Calibrate CI** as ADR-0208 decision 4 says. CI's rate becomes the NUC's 1× p50 divided by the
   runner's p50, rounded up, and the gate moves from p50 to p95. That is an amendment to ADR-0208,
   with the record from step 5 as its evidence.

A NUC that fails P14 is a finding to fix before the pilot. It is not a reason to loosen the budget.

## How to run P16 on the NUC

1. Start the store as it will run: the Windows service, a till, a KDS and a kiosk attached, and
   the pilot's package published to it.
2. Trade for eight hours, using the simulator's scenarios or a person working through the store's
   menu. Then leave it idle for ten minutes.
3. Sample `Working Set - Private` and `% Processor Time` of `pos_edge` every minute throughout.
   Record the maximum memory, the average CPU over the idle ten minutes, and the SQLite database and
   WAL file sizes at the start and at the end.
4. For the bundle: `pnpm build` in `ui/`, then the gzip size of every file under `ui/dist/assets/`.

## What blocks these gates, and what can be done before the hardware arrives

- **Build the till telemetry** (ADR-0197 decision 5, ADR-0208 decision 5). P15 needs it, and so does
  the console's "p95 per act per release" on a canary ring. It is the one code item here, and it
  belongs in the hardening work before the pilot.
- **Add the bundle check to CI** (P16's third threshold). It needs no hardware.
- **The P17 load driver is written** (`ui/perf/load-p17.mjs`, phase 10). Run it on the NUC against the
  store's real edge with `EDGE_URL`, `PAIRING_CODE`, `STAFF_CODE` and `STAFF_PIN`, for an hour
  (`DURATION_S=3600`), with a QR and intake load through the cloud beside it.
- **Buy or borrow the reference NUC** (an Intel or ASUS NUC, any Windows edition, as roadmap-v4
  "What the owner decided" says) and the pilot's tablet and phones. Everything else waits on them.
