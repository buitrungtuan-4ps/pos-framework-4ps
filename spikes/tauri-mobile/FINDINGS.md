# Spike: Tauri v2 on Android and iPad, feasibility only (throwaway, never merged)

Nothing was built. This container cannot build a mobile app, and this file records why and what
the real spike needs.

## What this container has and lacks (checked 2026-11-03)

| Need | Here |
|---|---|
| Rust target `aarch64-linux-android` | **installs** through the proxy (`rustup target add`, done) |
| JDK 17+ | OpenJDK 21 present |
| Gradle distributions, Maven Central | reachable (`services.gradle.org` 200; Maven Central 429 rate-limited) |
| Android SDK cmdline-tools, platform-tools, build-tools, **NDK** | **blocked**: `dl.google.com` is refused by the egress proxy. Without the NDK even `cargo check` fails: `ring` (via `ureq`/rustls) needs the NDK's clang for its C/asm |
| Google Maven (Android Gradle Plugin, AndroidX) | `maven.google.com` answers 200, but its artefacts are served from `dl.google.com` (blocked) |
| Xcode, iOS SDK, signing | impossible on Linux: needs macOS |
| A device or emulator | none; emulator needs KVM and ~8 GB, and the disk has ~6 GB free |

## What the app itself needs before it can run on mobile (read from `apps/pos-station`)

- **A library entry point.** Tauri mobile needs `[lib] crate-type = ["staticlib", "cdylib", "rlib"]`
  and `#[cfg_attr(mobile, tauri::mobile_entry_point)]`. Today it is a `main.rs` binary.
- **Tray** (`TrayIconBuilder`, `tray-icon` feature) is desktop-only: put it behind `cfg(desktop)`.
- **Print-agent sidecar** (`std::process::Command`) cannot run on Android or iOS. On mobile the
  device is a till only: printing goes through the edge or a network printer.
- **Credential store:** `keyring` has backends only for Windows, macOS and Linux in `Cargo.toml`.
  Android (Keystore) and iOS (Keychain, via the `apple-native` backend on iOS) need a mobile
  backend or a Tauri plugin (e.g. a stronghold or secure-storage plugin). That is a dependency, so
  it needs an ADR.
- **`tauri.conf.json`:** bundle targets are `deb`/`nsis`. Mobile needs `tauri android init` and
  `tauri ios init` (generated Gradle and Xcode projects), an Android `identifier` and versionCode,
  and a cleartext-HTTP exception (`usesCleartextTraffic` / ATS) because the edge is `http://` on the
  LAN (ADR-0111).
- **Window model:** Station/Terminal modes assume desktop windows. A mobile build has one webview,
  so it is "Terminal" with the till only.

## What the real spike needs (the ask)

1. **A runner with the Android SDK+NDK** (a GitHub-hosted `ubuntu-24.04` runner has them
   preinstalled) or a dev machine with Android Studio. Open egress to `dl.google.com` from this
   environment would also do. About 10 GB disk.
2. **A macOS runner with Xcode** (GitHub `macos-15`) for iPadOS, plus an **Apple Developer account**
   (team id, signing certificate, provisioning profile) to put a build on a real iPad. A Simulator
   build needs no account.
3. **A device:** one mid-range Android tablet and one iPad. Emulator figures do not count against
   10 ms, for the same reason the 4× throttle is only a stand-in.
4. The measurement to bring back: cold start to till drawn; the same `tap-to-paint` acts through
   the WebView (Android System WebView or WKWebView), run with the harness's probe injected by the
   shell's init script; memory; APK/IPA size.
5. Changes on a branch of `apps/pos-station`: lib entry point, `cfg(desktop)` around tray and
   sidecar, a mobile credential backend, and generated `gen/android` and `gen/apple` projects.
