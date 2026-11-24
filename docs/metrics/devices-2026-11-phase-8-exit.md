# Devices at the end of phase 8, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-24

The exit record of roadmap-v4 phase 8 (devices): what the exit criterion asked, what shows how far
it is met, what could not be verified without real hardware, and the owner's open decisions. Like
[templates-2026-11-phase-6-exit.md](templates-2026-11-phase-6-exit.md), it records one moment and is
not regenerated.

**Measured at** `5b08b316` on `main`, with #1339, #1354, #1356, #1357 and #1358 merged.

## The criterion

**The till runs on each device class in the priority order (Windows, tablet, Android, iOS, Linux),
and a card payment completes through the terminal port** ([ADR-0201](../adr/0201-the-till-runs-on-every-device-class.md),
[ADR-0230](../adr/0230-a-card-is-taken-through-the-terminal-port-and-an-unknown-result-parks-the-bill.md)).

## The criterion is partly met

- **A card payment completes through the terminal port: met against the fake.** The edge calls
  `PaymentTerminal` (`POST /api/bills/{id}/card`); a capture settles the bill, a decline records
  nothing, and an unknown result parks the bill until it is looked up, confirmed or cancelled, across
  a restart. The till's card button, the amber park with its three exits and a card at the kiosk run
  in the replay against the demo store's fake terminal (`ui/tests/card.spec.mjs`,
  `ui/tests/kiosk.spec.mjs`); a tab's bill pays through the same route, with no replay of its own. **No real terminal has taken a card**: B10.3's adapter waits on the
  owner's choice of vendor.
- **The till on each device class: not met on hardware.** The device class as a screen dimension
  works: `pnpm classes` (part of the till's build) passes, with 41 elements naming a device class and
  none a pixel width. On the shells:
  - **Windows (NUC):** kiosk behaviour that does not depend on the edition is built in POS Station
    (`"kiosk": true`: full screen, frameless, on top, not closable, no Quit) and tested on Linux CI;
    the manual check on a NUC (`docs/guides/pos-station.md`) has not been run.
  - **Tablet:** the till runs in a tablet's browser at the tablet class, as before; no tablet was
    tried.
  - **Android and iOS:** not built. Tauri v2 mobile needs POS Station split into a library with a
    mobile entry point, its desktop-only parts (tray, sidecar, OS keyring) behind `cfg(desktop)`, and
    a store for the token on the phone, a new dependency or native code whose options
    [ADR-0231](../adr/0231-a-phone-keeps-its-till-token-in-a-store-the-owner-chooses.md) (Proposed)
    sets out.
    The CI job (an Android build on ubuntu; iOS needs macOS runners) is written up, not added. Until
    then a phone runs the till in its browser, the fallback ADR-0201 decision 1 names.
  - **Linux:** POS Station is built, linted and tested on Ubuntu 24.04 in CI (`station.yml`); the till
    runs in Chromium in every replay. No Linux till machine was tried.

## What shipped

| Item | What | Pull requests |
|---|---|---|
| [ADR-0230](../adr/0230-a-card-is-taken-through-the-terminal-port-and-an-unknown-result-parks-the-bill.md) | The card through the terminal port, the park on an unknown result, a tab and the kiosk | #1339 |
| B9.2, the card | The edge calls the port; `billing.payment.captured` gains `terminal_reference`; the edge's fold counts only a captured payment; two catalogue acts; the till's card flow and park; the kiosk's card; the demo store's scripted fake terminal; unit tests and five replays | #1354 |
| Program C, the image | `deploy/edge-image/`: the container image around a released binary, a compose file for one store, a smoke test, and the `edge-image` workflow | #1356 |
| ADR-0201 decision 2, kiosk | POS Station's kiosk mode on any Windows edition, and the manual NUC check | #1357 |
| [ADR-0231](../adr/0231-a-phone-keeps-its-till-token-in-a-store-the-owner-chooses.md), **Proposed** | Where the Tauri mobile shell keeps a phone's till token: four options and their trade-offs, for the owner to choose; it unblocks the mobile build | #1358 |

## Decisions taken during the phase

- **The card is one route, not a tender on the payment route** (ADR-0230 decision 2): the edge mints
  the payment's id before it asks the terminal, so a retry cannot charge twice.
- **No new permission for the kiosk's card** (ADR-0230 decision 7): the kiosk is admitted to the card
  route only, for the whole of its own bill and no tip; the fixed `KIOSK` set is unchanged.
- **The demo terminal is scripted by `POS_DEMO_CARD`**, card by card, so the replay drives a decline
  and an unknown result; an unknown card is settled by the time it is looked up, as an acquirer's
  would be.
- **The image wraps the released binary rather than compiling one**, so the container runs the bytes
  a store PC runs and an update is a new image over the same volume (ADR-0113).

## Not verified without real hardware

Each is built and tested against fakes or on Linux CI, and not verified on hardware. Each has a gate
in [`gate-register.md`](../gate-register.md): P13 and P14 (the NUC, recorded by the phase 9
review), P20 to P23 (below):

- **A card terminal** (P20). No terminal exists to prove the adapter, the timeouts or the slip.
- **A NUC** (P13, P14). The kiosk mode, WebView2 (on LTSC too), autostart under a dedicated account, and the
  till's replay and tap budget (ADR-0197) on the reference box.
- **Phones and tablets** (P21). No Android or iOS build exists; no tablet ran the replay.
- **A scale** (P22). The `Scale` port has its fake and contract suite (phase 6) and no driver.
- **The container on a host** (P23). The image is smoke-tested in CI; the host agent that would start one
  per store is not built.

## Open owner decisions

- **Where a phone keeps its till token** ([ADR-0231](../adr/0231-a-phone-keeps-its-till-token-in-a-store-the-owner-chooses.md),
  Proposed): the webview's storage, Stronghold, a vetted Keystore/Keychain plugin, or a small plugin
  of our own. The mobile build waits on it.
- **The card-terminal vendor for Vietnam** (B10.3): the adapter goes under `crates/adapters/`
  (`cargo xtask new adapter --kind card`), passes the contract suite and brings the per-device
  terminal binding on the `devices` node.
- **Device purchases**: a NUC, an Android tablet and phone, an iPad, a card terminal and a scale for
  the verification above.
- **Card held on a tab** (pre-authorisation): needs a hold-then-capture in the port, decided with the
  vendor.

## What the phase leaves open

- Tauri mobile for Android and iOS, once ADR-0231 is decided.
- The host agent (`pos_host`, ADR-0113).
- Camera scanning: it would need a new dependency for the shell or the webview; deferred. A
  keyboard-wedge scanner works today.
- Real scale drivers.
- A console list of parked (unknown) card payments; the events carry them for the cloud.
- Bringing POS Station back after the process is killed, and leaving the native kiosk with a PIN.
