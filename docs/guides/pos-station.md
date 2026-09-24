# POS Station

**Status** Accepted · **Owner** @maintainers-architecture · **Last reviewed** 2026-09-24

POS Station is the app a store runs on its own PCs: a Tauri v2 shell that loads the till from the
store's edge, keeps the device's pairing in the operating system's credential store, and adds what a
browser tab cannot — a tray on the store PC, and the print agent on a second machine. The decision is
[ADR-0147](../adr/0147-pos-station-is-a-tauri-shell-over-the-edge.md) and its Amendment 1; it builds on
[ADR-0111](../adr/0111-a-second-origin-may-address-the-edge.md)'s credential seam and
[ADR-0112](../adr/0112-print-agents.md)'s print agent. This guide covers plan items 1.5 (the spike),
3.2 (Station) and 3.3 (Terminal).

## What it is

- **One app in [`apps/pos-station/`](../../apps/pos-station/), outside the Cargo workspace.** Its
  `src-tauri/Cargo.toml` has an empty `[workspace]` table and its own `Cargo.lock`, so no edge or
  cloud binary takes the webview's dependency tree and `cargo deny` over the workspace is unchanged.
  `just preflight` does not build it; the commands below do.
- **The till comes from the edge's own URL.** The main window opens `http://<edge>/`, so the till is
  always the version its edge serves and every request is same-origin — no allow-list, `/ws` or
  `pos-proto` change.
- **Two bundled pages, no build step.** `ui/connect.html` and `ui/status.html` are plain HTML, CSS
  and JavaScript. Their strings are keys into `ui/i18n.js` (English and Vietnamese, chosen by
  `navigator.language`); the tray and notifications have their own table in `src-tauri/src/i18n.rs`,
  with no key in common.

| File | Job |
|---|---|
| `src-tauri/src/station.rs` | state, windows, tray, the start-up decision, pairing gained and lost |
| `src-tauri/src/monitor.rs` | the fifteen-second round against the edge |
| `src-tauri/src/health.rs` | pure: what a round learned, and which changes are notifications |
| `src-tauri/src/address.rs` | pure: an address or a pairing link, understood |
| `src-tauri/src/init_script.rs` | pure: the script that hands the till its token |
| `src-tauri/src/sidecar.rs` | the print agent's supervisor |
| `src-tauri/src/commands.rs` | the four commands, and who may call them |
| `src-tauri/src/edge.rs`, `vault.rs`, `config.rs` | the edge calls, the credential store, `station.json` |
| `src-tauri/capabilities/local-pages.json` | the only capability |
| `src-tauri/tauri.conf.json` | the app, its CSP, and the bundles (`.deb`, NSIS) |
| `src-tauri/tauri.sidecar.conf.json` | adds the print agent to a bundle (see *Build it*) |

## The two modes

| Mode | Chosen when | What it adds |
|---|---|---|
| **Station** (3.2) | `http://127.0.0.1:8080/healthz` answers at start, or the device paired with a loopback address | a tray with a one-line summary, **Open till**, **Status**, **Pairing QR** (the till's Devices screen, where a manager mints a code and its QR) and **Quit**; a desktop notification for each change listed below |
| **Terminal** (3.3) | anything else: the edge is another machine | the print agent as a sidecar started with this terminal's token; a smaller tray (**Open till**, **Status**, **Quit**) |

Both open the till when the app starts, **full screen** unless `till_full_screen` is `false` in
`station.json`. The installers start the app at every login (a `/etc/xdg/autostart` entry from the
`.deb`, an `HKLM\…\Run` value from the NSIS hook). The mode is decided once per run; the loopback
clause keeps a store PC a Station when the app starts before the edge's service does.

### Pairing

1. With no pairing, the app opens **connect**. It takes the edge's address — `192.168.1.10:8080`, or
   the pairing link `http://192.168.1.10:8080/pair?code=123456`, or ADR-0111's hosted
   `https://till.example/pair?code=123456` — and the six-digit code, which a pasted link already
   carries.
2. The Rust side posts the code to `POST /api/pair`, stores the token in the OS credential store
   (service `pos-station`, account the edge's origin as a browser spells it) and the origin in
   `station.json`.
3. The till window opens with an **initialization script** that, before any of the till's own code
   runs, writes `localStorage["pos-edge.device-token"]` and removes `pos-edge.base-url` — exactly the
   two keys [`credentials.ts`](../../ui/src/api/credentials.ts) reads — so the till starts paired and
   its requests stay root-relative. The script writes nothing unless `window.location.origin` is that
   edge.

If the credential store refuses the token, the pairing still succeeds for this run — the code is
single-use and already spent — and the status page says it will not survive a restart. That is the
till's own rule: a device that cannot persist its token pairs again next session.

Every round also asks `GET /api/pair/devices`, which sits behind the paired-device gate only. A `401`
there means the edge no longer knows the token (revoked, or an edge whose registry did not survive a
restart): the app forgets it, stops the print agent, closes the till and reopens **connect** with a
notice.

### What the tray knows, and what it cannot

Every fifteen seconds the monitor asks `/healthz` (up or down, and the version) and
`/api/pair/devices` (still paired). `/api/sync` (cloud link, outbox depth and level) and
`/api/printers` sit behind the **signed-in** gate as well, and every answer resets the signed-in
person's thirty-minute idle window ([ADR-0091](../adr/0091-durable-edge-auth-state.md)). So the
monitor asks them **only while a till window is open** — the till's own status bar already polls
`/api/sync` every fifteen seconds then, so the tray adds nothing — and with the till closed the tray
says *Open the till to see the cloud link* rather than keep somebody signed in. With nobody signed
in, the edge answers `403` and the tray says so.

Notifications (Station mode): the store server stops or starts answering; the pairing is lost; the
cloud link goes from online to offline and back; the outbox rises past half, four fifths or all of the
planned depth and falls back to normal (the edge reports `outbox_planned_depth` and `outbox_level`,
[ADR-0137](../adr/0137-a-deep-outbox-warns-and-never-refuses.md)); a printer joins or leaves the
published list. The first reading after start, or after the till was closed, is a baseline and not a
notification — except a lost pairing, which is news whenever it is first seen.

**A printer going offline cannot be reported.** `/api/printers` lists what the store published — an
id, a name, a station — and nothing about reachability. The tray reports the list changing; online
and offline need an edge route first.

Linux's AppIndicator shows no tooltip, so the summary is also the first, disabled, menu item.

### The print agent (Terminal)

The bundle ships `pos_print_agent` beside the app's executable (`bundle.externalBin`), and the app
starts it the way its installers do (ADR-0112): a `print-agent.toml` in the app's data directory
naming `edge_url` and `state_path` (rewritten at every start), the token in `POS_PRINT_AGENT_TOKEN`
(never in the file), and its durable log in `POS_PRINT_AGENT_LOG_FILE` under the app's log directory.
It is restarted when it exits — after 1 s, doubling to 60 s, back to 1 s after a run of a minute —
and killed on **Quit**; a job it was printing returns to the queue at its lease.

The agent prints for a terminal only after a manager binds it at the till (`POST /api/print/agent`).
Until then the edge answers `409` and the agent asks again every five seconds, which is the right
state for a freshly paired terminal. If the app is killed rather than quit, the agent outlives it
until the next start (see *What is left*).

## Security

- **Remote pages get no commands.** `capabilities/local-pages.json` grants the four commands
  (`set_language`, `connect_context`, `pair`, `status`) to the `connect` and `status` windows on local
  URLs, and nothing else to anything. `build.rs` declares the commands in an app manifest, which makes
  that grant required. Each command also checks its caller's window and URL. The till's window
  appears in no capability, Tauri 2.11 refuses a remote origin that no capability names, and
  `withGlobalTauri` is off.
- **The till stays on its edge.** Its window refuses navigation to any other origin, and the
  initialization script writes the token only on the edge's origin.
- **The bundled pages have a CSP**: `default-src 'self'; script-src 'self'; style-src 'self';
  img-src 'self' data:; connect-src ipc: http://ipc.localhost; object-src 'none'; base-uri 'none';
  form-action 'none'; frame-ancestors 'none'`.
- **No token in a file or a log.** It is in the credential store and, for the agent, its environment;
  every type that holds one redacts it in `Debug`.
- **No proxy.** The app calls the edge directly whatever `HTTP(S)_PROXY` says; a LAN request should
  never leave the LAN.

The app does not use the workspace's `KeyVault` port, which ADR-0111 sketched for a shell: ADR-0147
put the app outside the workspace, and a new `SecretName` variant would be a port change needing its
own ADR. It uses the same `keyring` crate with the same backends as
[`key-vault-keyring`](../../crates/adapters/key-vault-keyring/src/lib.rs)
([ADR-0086](../adr/0086-edge-keyvault-and-activation.md)) instead.

## Build it

### Linux (Ubuntu 24.04)

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev libxdo-dev
```

The toolchain is the repository's pin (the root `rust-toolchain.toml` applies inside `apps/`), and
the Tauri CLI comes from npm, so Node 22 is the only other requirement.

```bash
cd apps/pos-station/src-tauri
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

The app restates the workspace's lint rules in its own `[lints]` table, and the root `clippy.toml`
applies because clippy finds it by walking up. The one exception is `main.rs`'s `context()`, which
lifts three bans for the code `tauri::generate_context!` expands to.

### The print agent binary

From the repository root:

```bash
cargo build --release -p pos-print-agent
mkdir -p apps/pos-station/src-tauri/binaries
cp target/release/pos_print_agent \
   apps/pos-station/src-tauri/binaries/pos_print_agent-x86_64-unknown-linux-gnu
```

On Windows the file is `target\release\pos_print_agent.exe`, copied to
`binaries\pos_print_agent-x86_64-pc-windows-msvc.exe`. `binaries/` is not committed.

The agent is listed in `tauri.sidecar.conf.json`, not in `tauri.conf.json`, because `tauri-build`
refuses to compile an app whose `externalBin` is missing — which would make `cargo test` and
`cargo clippy` need the agent too. Only the bundling command below merges it.

### Bundle

```bash
cd apps/pos-station
npx @tauri-apps/cli@2.11.5 build --config src-tauri/tauri.sidecar.conf.json --bundles deb
```

The package lands in `src-tauri/target/release/bundle/deb/`. For a quick local run,
`build --debug --no-bundle` with the same `--config` puts `pos-station` and the agent side by side in
`src-tauri/target/debug/`.

### Windows

On Windows 10 or 11 with the MSVC build tools, the pinned Rust toolchain and Node 22:

```powershell
cargo build --release -p pos-print-agent
copy target\release\pos_print_agent.exe apps\pos-station\src-tauri\binaries\pos_print_agent-x86_64-pc-windows-msvc.exe
cd apps\pos-station
npx @tauri-apps/cli@2.11.5 build --config src-tauri/tauri.sidecar.conf.json --bundles nsis
```

**WebView2.** `bundle.windows.webviewInstallMode` is `embedBootstrapper`: the installer carries
Microsoft's small bootstrapper and installs the Evergreen runtime when a machine lacks it, which needs
the internet at install time. **LTSC and offline stores** ship without WebView2 and may have no
internet, so for them bundle a fixed runtime instead: download the *Fixed Version* runtime from
Microsoft, expand the `.cab` into `src-tauri/`, and set

```json
"webviewInstallMode": {
  "type": "fixedRuntime",
  "path": "./Microsoft.WebView2.FixedVersionRuntime.<version>.x64/"
}
```

That adds about 180 MB to the installer and makes the fork responsible for updating the runtime,
because a fixed runtime never updates itself. `offlineInstaller` (the full Evergreen installer inside
ours) is the middle choice.

**Autostart.** `windows/installer-hooks.nsh` writes `HKLM\Software\Microsoft\Windows\CurrentVersion\Run`
→ `POS Station` on install (a per-machine installer, so every user of the store PC) and removes it on
uninstall.

## Sign it (Windows)

`bundle.windows.signCommand` runs [ADR-0142](../adr/0142-windows-signing-is-the-forks-choice.md)'s
[`sign-windows.ps1`](../../deploy/release/sign-windows.ps1) — the same script the release workflow
uses, so the two cannot drift — once for each file the bundler signs (the app, the sidecar, the
installer):

```text
powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass
  -File ../../../deploy/release/sign-windows.ps1 -Path %1
```

The path is relative to `apps/pos-station/src-tauri`, where the CLI runs the bundler. The script
reads the fork's choice from the environment of the `tauri build`:

| Set | Result |
|---|---|
| nothing | a notice, and an unsigned installer (SmartScreen warns on first run) |
| `POS_SIGN_PFX_BASE64`, `POS_SIGN_PFX_PASSWORD` | signed with that certificate — an internal one from `new-internal-signing-cert.ps1`; a public CA no longer issues a `.pfx` ([release runbook](../release-runbook.md#authenticode-signing-the-windows-binary-for-windows-itself)) |
| `POS_SIGN_COMMAND` (with `{file}`) | signed by that command: a public CA's certificate through its signing service (Azure Trusted Signing, DigiCert KeyLocker, SSL.com eSigner), an EV token, a KMS through `jsign` |
| `POS_SIGN_REQUIRED=true` | no signing configured fails the build instead |

The app has no over-the-air update of its own yet, so there is no minisign step to order it against.

## Run it

- **Settings**: `station.json` in the app's configuration directory
  (`~/.config/com.pizza4ps.pos-station/`, `%APPDATA%\com.pizza4ps.pos-station\`): `edge_origin`,
  `till_full_screen` (default `true`) and `language` (the webview's, reported by the bundled pages).
- **Token**: the OS credential store — Windows Credential Manager, or on Linux the kernel keyring,
  which survives app restarts and logouts but **not a reboot**, so a Linux terminal pairs again after
  one. The kernel keyring also needs the login's session keyring (PAM `pam_keyinit`); where there is
  none, the pairing lasts until the app quits and the status page says so.
- **Logs**: stderr, with `POS_STATION_LOG=debug` for one line per round; the agent's own log is
  `print-agent.log` in the app's log directory.
- **One copy per user**: a second start finds `station.lock` held and exits, so there is never a
  second tray or a second agent.

## Dependencies

| Crate | Why |
|---|---|
| `tauri` 2 (`tray-icon`, `image-png`), `tauri-build` 2 | the shell, the tray, and decoding the tray's PNG |
| `tauri-plugin-notification` 2 | desktop notifications (D-Bus on Linux, toast on Windows) |
| `serde`, `serde_json` | command payloads, `station.json`, the edge's JSON |
| `ureq` 3 (`rustls`, `json`) | the five edge calls. Synchronous, on the app's own threads, so no async task blocks; ring and the bundled Mozilla roots, as in the workspace's own client |
| `keyring` 3 | the credential store, with `key-vault-keyring`'s backends |
| `log` | the facade Tauri already logs through; `logging.rs` is a small stderr sink |

Deliberately not taken: `tauri-plugin-shell` (the sidecar is spawned with `std::process`, and the
plugin's JavaScript API is exactly what the pages must not have), `reqwest` (its async stack buys
nothing for five calls on dedicated threads), `tokio` as a direct dependency (waits are channel
timeouts), `tauri-plugin-single-instance` (`File::try_lock`), `tauri-plugin-autostart` (the
installers own autostart) and `tauri-plugin-log`.

## Spike results (plan item 1.5)

Measured on 2026-09-24 in an Ubuntu 24.04 container: Xvfb at 1600×1000 with **no GPU** (WebKitGTK
2.52.6 renders in software), stalonetray as the tray host, openbox as the window manager for the
kiosk and release runs, a private D-Bus session, and `examples/minimal-edge` as the edge. The
Terminal run used an edge on the machine's LAN address. Memory and start-up figures are for the
release build unless marked.

| Question | Result |
|---|---|
| Pairing from the connect page | works, with a typed code and with a pasted pairing link |
| The till loads already paired | yes: it routes to `/signin`, not `/pair`, and shows *Store connected* (its `/ws` passed the paired-device gate with the handed-over token); sign-in and the floor work in the webview |
| Pairing survives an app restart | yes; the second start opened the till directly |
| Tray and health poll | icon, menu and summary update every round; `/healthz` and `/api/pair/devices` every 15 s; `/api/sync` and `/api/printers` only with a till open |
| Notifications | fired on D-Bus: store server not responding, store server is back, no longer paired (no daemon was running to display them) |
| Pairing lost | noticed within one round; till closed; connect reopened with its notice |
| Edge down at start | the status page says so while the app polls every 3 s; when an edge answered, the till opened and the status page closed |
| Print sidecar | started with the terminal's token (the edge accepted it and answered `409`, not bound); restarted after 1, 2 and 4 s when killed; stopped on **Quit** |
| Remote page gets no commands | a hostile page served as the till was refused all four commands, the notification plugin and `core:event`; its navigation to another origin was blocked |
| Kiosk | under a window manager the till opens full screen (1600×1000 at 0,0, no decorations); closing it leaves the app in the tray, and **Open till** brings it back full screen. Bare Xvfb, with no window manager, does not honour the request |
| Idle memory | 551 MiB RSS (app 168, WebKitWebProcess 327, WebKitNetworkProcess 56) and 378 MiB PSS, 52 s after start with the till full screen. With `WEBKIT_DISABLE_COMPOSITING_MODE=1`: 380 MiB RSS and 223 MiB PSS (web process 161 MiB) |
| Idle memory, debug build | 534 MiB RSS, 366 MiB PSS (1280×800 window) |
| Start to a usable till | 0.55 s from launch, on a paired machine, until the till requested its sign-in page (three launches: 0.57, 0.53, 0.55 s); the till's first page was requested at 0.35–0.37 s. Debug build: 1.15 s. Times are from the edge's request log |
| Installer size | `.deb` 4.4 MiB (4,591,736 bytes), installing 10.1 MiB: the app 6.7 MiB and the release print agent 3.3 MiB, both stripped. WebKitGTK is a system dependency (`libwebkit2gtk-4.1-0`), not bundled |

Most of the web process's memory here is WebKit's compositing path running on software GL: with
compositing switched off it halves, and the till looked the same. That variable is a knob for a
Linux box without a usable GPU, not a default — on a machine with one, compositing is what keeps
scrolling smooth.

**Not measured here, and why:**

- **Windows** — WebView2 on LTSC or anywhere else, the NSIS installer and its autostart hook, and
  signing. There is no Windows machine in this environment, so ADR-0147's LTSC question stays open.
- **Real printers** — the demo store publishes no printers and no terminal, so no agent was bound and
  no job printed.
- **A GPU, a notification daemon, a desktop session** — the memory figures are for software
  rendering, notifications were observed on the bus rather than on screen, and the session keyring
  came from a helper standing in for a login.
- **A first launch after boot** — the start-up figures are warm-cache launches.
- **A Linux reboot** — expected to lose the pairing (kernel keyring); not exercised.
- **macOS** — not a target.

## What is left

- **Hardware for 3.3, out of scope per ADR-0147**: the customer display, the scale, the cash drawer
  and card-terminal SDKs. Each needs hardware to prove against, and some need a port and an adapter.
  A keyboard-wedge scanner already works in the webview.
- **A CI job** that builds and tests the app: a `.github` change, which needs an owner review
  ([ADR-0126](../adr/0126-when-an-agent-may-merge.md)).
- **Printer online/offline** in the tray: needs an additive edge route.
- **A persistent Linux credential store**: the Secret Service would survive a reboot but needs a
  desktop session running one — a decision, not a fix.
- **Windows verification**: WebView2 on LTSC, the installer, the autostart hook and signing.
- **An agent that outlives a killed app**: a Job object on Windows and `PR_SET_PDEATHSIG` on Linux,
  both `unsafe` FFI this crate denies today.
