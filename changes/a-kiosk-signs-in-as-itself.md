---
kind: added
upgrade_note: Adds the permission `admin.kiosk.exit` (PIN-flagged; MANAGER and OWNER by default for new tenants), the setting `kiosk.abandon_seconds` (30 to 900, default 120), the framework reason code `ABANDONED` (void a line, close an order) Migration 0104 grants `admin.kiosk.exit` once to every existing role on the terms it grants `admin.device.manage`: directly where that is held directly, and with approval otherwise. A store that publishes its own reason list keeps it, and a kiosk there voids an abandoned order only where the list names `ABANDONED` or asks no reason.
---
- **A kiosk signs in as itself, and a person leaves it with a PIN** (ADR-0224 decision 4). A till on
  a terminal the console makes `DEVICE_CLASS_KIOSK` signs in through `POST /api/session/kiosk` as
  the kiosk's own record, which the store derives from the published `devices` node with the
  terminal's id, no code to type and no PIN; any other device is refused `403`. The record holds only the fixed kiosk set,
  `sales.line.add` and `billing.bill.open`, whatever its roles say, and no PIN can step it past
  that. A person signing in on a kiosk with their code and PIN leaves it, and is refused `403`
  `KIOSK_EXIT_NOT_ALLOWED` unless they hold the new `admin.kiosk.exit`. `GET /api/session` carries
  `kiosk_abandon_seconds`, from the new setting `kiosk.abandon_seconds`, and the framework's reason
  list gains `ABANDONED` ("Guest walked away") for the kiosk's abandoned orders.
