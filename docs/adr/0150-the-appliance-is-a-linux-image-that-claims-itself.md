# ADR-0150 — The appliance is a stock Linux image that claims itself, and Android waits for a device

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-09-24
· Relates to [ADR-0148](0148-an-unclaimed-box-shows-a-code-and-the-console-claims-it.md),
[ADR-0147](0147-pos-station-is-a-tauri-shell-over-the-edge.md), [ADR-0003](0003-cattle-not-pets.md)

## The problem

Plan item 4.4 asks for two long-term options: a Linux appliance (a small PC that is the store server
and, if it has a screen, the till) and a Tauri spike on Android.

The edge already runs as a systemd service (`deploy/edge/pos-edge.service`). What an appliance lacks:

- one repeatable way to turn a stock Debian or Ubuntu install into a store box;
- a kiosk for the screen;
- a first boot that needs no store-specific image.

## Decision

- **`deploy/appliance/provision.sh`** turns a stock Debian 12 or Ubuntu 24.04 install into a store box.
  It is idempotent and needs root. It:
  - creates the `pos-edge` user;
  - puts the binary in the bin slots the updater already uses (`/var/lib/pos-edge/bin/current`);
  - installs `fonts-dejavu-core` and the service unit;
  - with `--kiosk`, adds an autologin **cage + Chromium** session that opens the till full screen on
    `http://127.0.0.1:8080`.
- **The image is generic.** It carries no `config.toml`. A first-boot unit runs
  `pos-edge claim --cloud <url>` (ADR-0148) and starts the edge once the console has claimed the box.
  The same image serves every store, and a stolen image claims nothing.
- **`deploy/appliance/cloud-init.yaml`** does the same for a mini-PC or VM provisioned with cloud-init.
- **CI parses the scripts** with `bash -n`, next to the PowerShell checks, through the same
  `installer-syntax.mjs --emit`.
- **Android stays a spike, and this record says what it must show before it becomes a decision:**
  - a Tauri v2 Android build of ADR-0147's app pairs natively;
  - it holds the till full screen with screen pinning;
  - it survives a day of service on a mid-range tablet;
  - its token outlives an app update.

  Until then an Android till is Chrome on the LAN. That already works, but it is not a secure context
  over plain `http`, so it cannot be installed as an app. `public_origin` over https is the way to
  make it one.

## Consequences accepted

- **The appliance is a script and a cloud-init file, not a published image.** Publishing one means
  hosting, signing and patching an OS, a supply-chain commitment this record does not make. A fork
  builds its own image by running the script in its imaging tool.
- **The kiosk's browser is the distribution's Chromium**, updated by the OS rather than by our
  updater. Only the edge rides our signed updates.
- **Android is unproven until someone runs it on a device.** Nothing here claims otherwise.
