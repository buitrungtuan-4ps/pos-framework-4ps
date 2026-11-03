# ADR-0201 — The till runs on every device class, through one shell

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Extends
[ADR-0147](0147-pos-station-is-a-tauri-shell-over-the-edge.md) (to Android, iOS and kiosk
behaviour) · Relates to [ADR-0111](0111-a-second-origin-may-address-the-edge.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0190](0190-a-screen-is-drawn-from-the-package.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner wants the till on every device, in this order: Windows, tablet, Android, iOS, Linux. The
reference box is an Intel or ASUS NUC mini PC, on any Windows edition: Home, Pro or Enterprise.

What exists:

- **POS Station** ([ADR-0147](0147-pos-station-is-a-tauri-shell-over-the-edge.md)), a Tauri v2 app
  outside the workspace, loads the till from its edge's own URL, pairs natively and keeps the token
  in the OS keychain. It runs as Station (the edge on the same box) or Terminal (the edge elsewhere).
  Its Android spike has not been run.
- **Four device classes** in the till (`ui/scripts/device-classes.mjs`, `docs/ui-ux.md`): phone,
  tablet, terminal and kitchen display, enforced as named classes rather than pixel widths.
- **Windows lockdown** through Shell Launcher or Assigned Access exists only on Enterprise and IoT
  Enterprise editions, not on Home or Pro. Tauri 2.12 no longer supports Windows 7.

## Options considered

1. **A browser on every device, no app.** It works today, but nothing starts it, holds it full
   screen, or keeps its token outside browser storage.
2. **A native app per platform.** Several code bases for one till.
3. **One Tauri v2 shell on every platform, over the till its edge serves** (chosen).

## Decision

1. **Android and iOS are Tauri v2 mobile builds of POS Station**, in Terminal mode only (the edge is
   always elsewhere). The window loads the till from the edge's URL as on desktop; the token is kept
   in the Android Keystore or the iOS Keychain. Phase 0's spike decides whether Tauri mobile is fit;
   if it is not, the fallback is the till as an installed web app, and this record is amended.
2. **Kiosk behaviour does not depend on the Windows edition.** POS Station itself provides it on
   every edition, Windows 10 and 11, LTSC included:
   - starts at sign-in of a dedicated local Windows account, with automatic sign-in documented;
   - holds a frameless, full-screen window and comes back if it is closed or crashes;
   - leaves kiosk only with a manager's PIN;
   - on Enterprise and IoT Enterprise, Shell Launcher or Assigned Access are documented as extra
     hardening, never required.
3. **The device class is a published fact.** Each paired device gets a `device_class` on the
   `devices` node: `DEVICE_CLASS_TERMINAL`, `_TABLET`, `_PHONE`, `_KITCHEN_DISPLAY`, and `_KIOSK` for
   a guest-facing self-order device, with `DEVICE_CLASS_UNSPECIFIED` meaning "read it from the
   screen" through the existing named breakpoints. It is the device dimension of
   [ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md) decision 4 and the class
   [ADR-0190](0190-a-screen-is-drawn-from-the-package.md) checks components against.
4. **The order of work is the owner's:** Windows on the NUC, then tablets, Android, iOS and Linux.
   Each class is done when the till passes ADR-0109's replay and
   [ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)'s budget on a real device of that
   class, before phase 9.
5. **Not here:** the card terminal, scales and the edge's container image with its host agent. Each
   needs hardware to prove against and arrives in phase 8 with its own record.

## Consequences accepted

- **Additions to `pos-proto`:** an optional `device_class` on the `devices` node, additive.
- **A Home or Pro box can be left by someone who knows Windows well.** Kiosk behaviour makes that
  deliberate, not accidental; real lockdown needs an Enterprise edition, and the docs say so.
- **The mobile apps need store accounts and signing**, a release-owner task like ADR-0147's desktop
  build; no `.github` change is made without an owner review.
- **WebView differences** (WebView2, Android System WebView, WKWebView) are tested by running the
  till's replay on each class, not by assuming one engine.
