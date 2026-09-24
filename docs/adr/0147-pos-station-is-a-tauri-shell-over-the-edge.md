# ADR-0147 — POS Station is a Tauri v2 shell around the edge's own UI

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0111](0111-a-second-origin-may-address-the-edge.md), [ADR-0112](0112-print-agents.md),
[ADR-0140](0140-a-store-pc-installs-itself-from-one-file.md), [ADR-0142](0142-windows-signing-is-the-forks-choice.md)

## The problem

A browser tab makes a poor till and an invisible server.

- **The till.** Nothing starts it at boot, holds it full screen, or keeps its token anywhere but the
  browser's storage.
- **The store PC.** Nobody at the store can see whether the cloud link, the printers or the outbox
  are healthy without opening a console they have no login for.
- **A second PC.** It needs a separate print-agent install to reach its printer.

Plan items 1.5 (spike), 3.2 (Station) and 3.3 (Terminal) ask for one native app. Two findings shape it:

- Tauri's webview origin (`tauri://localhost`, or `http://tauri.localhost` on Windows) is not an
  `http(s)` origin that ADR-0111's allow-list or the `/ws` check accepts.
- The UI is compiled into every edge (ADR-0111), so a second copy in an app would have to be kept in
  step with every edge version.

## Decision

- **One app, `apps/pos-station`**, built with Tauri v2. It sits **outside the Cargo workspace** with
  its own lockfile, so no edge or cloud binary takes its dependency tree, and `cargo deny` on the
  workspace does not change.
- **Its window loads the till from the edge's own URL.** The page is same-origin with the edge, so no
  allow-list, `/ws` or `pos-proto` change is needed, and the till is always the version its edge
  serves. The app bundles only two small pages of its own: **connect** and **status**.
- **Remote pages get no Tauri commands.** Everything native is driven from the app's Rust side. The
  capability file grants commands only to the two bundled pages.
- **Pairing is native.** The connect page takes the edge's address and the six-digit code (or the
  pairing link from the QR). The Rust side calls `POST /api/pair`, keeps the token in the **OS
  keychain**, and hands it to the till page through an initialization script that writes it where
  the till's `CredentialStore` reads it (ADR-0111's seam). The till starts already paired, and a
  reinstall does not lose the pairing.
- **Two modes, chosen by where the edge is:**

  | Mode | When | Adds |
  |---|---|---|
  | **Station** (3.2) | the edge answers on `127.0.0.1` | tray icon with the cloud link, outbox depth and printers; a notification when one of them changes; the status page and the pairing QR from the tray; the till full screen at login |
  | **Terminal** (3.3) | the edge is another machine | the till full screen, and the print agent as a **sidecar** started with the terminal's own token, restarted if it dies |

- **Windows installers embed the WebView2 bootstrapper.** For LTSC and offline stores, which ship
  without WebView2, the build can bundle a fixed runtime instead. Installers are signed with ADR-0142's
  `sign-windows.ps1` as the `signCommand`.

## Consequences accepted

- **The app is built by the release owner, not yet by CI.** A build job is a `.github` change, which
  needs an owner review (ADR-0126). Until then the app is built with the documented steps, and the
  pull request that adds it records what was measured and on which OS.
- **Not in this record:** the customer display, scale, cash drawer and card-terminal SDKs. Each needs
  hardware to prove against, and some need a port and an adapter. A keyboard-wedge scanner already
  works in the webview.
- **The spike's WebView2-on-LTSC question stays open** until the app runs on an LTSC machine. The
  Linux build answers the rest: memory, installer size, kiosk and the print sidecar.
