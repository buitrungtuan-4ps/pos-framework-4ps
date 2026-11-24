# ADR-0231 — A phone keeps its till token in a store the owner chooses

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-24 · Waits on the
owner's choice among the options below · Extends
[ADR-0201](0201-the-till-runs-on-every-device-class.md) decision 1 · Relates to
[ADR-0147](0147-pos-station-is-a-tauri-shell-over-the-edge.md) (Amendment 1, `keyring`),
[ADR-0111](0111-a-second-origin-may-address-the-edge.md) (the till's credential seam),
[ADR-0086](0086-edge-keyvault-and-activation.md), [ADR-0084](0084-device-authentication.md)

Written on 2026-11-24 at the coordinator's request, during phase 8, to unblock the mobile step
without deciding it. **The owner chooses**; this record is marked Accepted when they do, and says
which.

## Context

ADR-0201 decision 1 makes Android and iOS Tauri v2 mobile builds of POS Station, in Terminal mode,
"the token kept in the Android Keystore or the iOS Keychain". Phase 8 did not build them
(its exit record, `docs/metrics/devices-2026-11-phase-8-exit.md`). The blocker is narrow: POS Station keeps
the device token with `keyring` 3 (ADR-0147 Amendment 1), whose backends are Windows Credential
Manager, the macOS Keychain and the Linux kernel keyring. It has no Android backend, and the app
enables none for iOS (whether its Apple backend serves iOS is to be confirmed), so the mobile build
needs another store, and every candidate is a new dependency or new native code, which
AGENTS.md §7 puts behind an ADR.

What the token is: a paired device's bearer credential for its own store's edge (ADR-0084). Whoever
holds it can act as that device on the store's LAN until a manager revokes it; a person must still
sign in with a code and PIN to act as anyone. It is not card data and not personal data. Losing it
costs a re-pair (a manager's six-digit code), not money.

## Options

1. **The webview's own storage** (the till's default `CredentialStore`, ADR-0111). No new
   dependency and no native code; it is what a phone running the till in its browser does today.
   Any script running in the till's origin can read it, and on a rooted or jailbroken device so can
   anyone with the file. It is not what ADR-0201 decision 1 says, so choosing it amends that record.
2. **An official Tauri plugin that encrypts at rest without the platform's key store**
   (`tauri-plugin-stronghold`, to be confirmed against its current mobile support). One Tauri-org
   dependency; the vault is encrypted with a key the app must derive from something, and on a till
   with no person to type a password that something lives on the device, so the protection is
   close to option 1's against a rooted device, and better against a copied backup.
3. **A community Tauri plugin over the Android Keystore and the iOS Keychain**. The protection
   ADR-0201 asks for, for the cost of one third-party dependency, with Kotlin and Swift inside it.
   Which plugin, its maintenance and its licence are **to be vetted** before this option is
   chosen; none is named here because none has been reviewed.
4. **A small plugin of our own in `apps/pos-station`**: Kotlin over the Android Keystore
   (a key held by the Keystore encrypting the token in the app's private storage) and Swift over
   Keychain Services, behind one Rust trait the desktop `keyring` path also implements. No
   third-party crate; about two small native files and their review, which nobody on the team has
   yet done for this app, and a release-owner build on macOS for iOS.

## What each costs

| | New dependency | Hardware-backed key | Survives a reinstall | Review burden |
|---|---|---|---|---|
| 1 webview storage | none | no | no | none |
| 2 Stronghold | one, Tauri org | no | no | low |
| 3 community plugin | one, third party | yes (Keystore / Keychain) | no on Android; Keychain items may on iOS | vet the plugin |
| 4 own plugin | none (native code instead) | yes | as 3 | review our Kotlin and Swift |

All four keep the token off the edge's logs and out of any event (AGENTS.md §2), and all four keep
pairing native (ADR-0147): only where the token rests changes.

## Recommended, not decided

Option 4 if the team will own two small native files; option 3 if a vetted plugin exists; option 1
only as a stated stopgap for a pilot on store-owned, managed devices. The owner chooses.

## Consequences, whichever is chosen

- The mobile build can start: POS Station becomes a library with a mobile entry point, its tray,
  sidecar and `keyring` behind `cfg(desktop)`, Terminal mode only (ADR-0201 decision 1).
- CI gains an Android build on an Ubuntu runner; iOS needs a macOS runner and a signing identity,
  which is the release owner's (ADR-0201, Consequences).
- No change to the edge, `pos-proto`, a port or the workspace's dependencies: the app is outside the
  workspace (ADR-0147).
- **Data:** the token is neither personal data nor card data; no PDPD (Decree 13/2023) question
  arises from where it rests.
