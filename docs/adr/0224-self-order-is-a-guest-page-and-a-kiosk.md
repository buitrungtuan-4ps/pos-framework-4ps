# ADR-0224 — Self-order is a guest page the cloud serves and a kiosk that signs in as itself

**Status** Accepted 2026-11-18 (owner) · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Needed by [ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md) decision 5 ·
Completes [ADR-0012](0012-qr-ordering-via-cloud.md) and [ADR-0057](0057-qr-ordering.md) for the
guest's side · Relates to [ADR-0116](0116-the-qr-hold-is-derived-and-it-gates-firing.md),
[ADR-0155](0155-a-qr-payment-is-confirmed-by-its-gateway-through-the-cloud.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0201](0201-the-till-runs-on-every-device-class.md), ADR-0222

Accepted by the owner on 2026-11-18 as recommended: the cloud hosts the guest page, which collects
nothing personal and is reviewed for security in phase 9; a kiosk order is paid at the counter, and
by card at the kiosk once the card terminal arrives in phase 8; a kiosk signs in as a staff record of
its own with a `KIOSK` role, and leaving the kiosk screen needs a manager's PIN.

## Context

QR ordering's intake is built: `POST /v1/qr/orders` with a signed table token and its guardrails
(ADR-0057), and staff confirmation on the till (ADR-0116). A guest has nothing to order *from*: no
menu read and no page. A kiosk is a device class (ADR-0201) with nothing guest-facing on it, and
every act on the edge names a person (`Actor.employee_id`), so a kiosk with nobody signed in can do
nothing.

## Options considered

1. **The store serves the guest page.** Guests are on mobile data; a store only dials out (ADR-0012).
2. **A device-only actor in the core.** Every permission check and audit gains a second kind of
   actor.
3. **A cloud-served guest page; a kiosk that signs in as a staff record of its own** (chosen).

## Decision

1. **The guest menu.** `GET /v1/qr/menu` with the signed table token answers the store's published
   menu for its QR channel: names in the guest's language, prices, availability, modifier groups.
   The same guardrails as the intake (business hours, store offline, rate limit) apply.
2. **The guest page** is a small front end in the repository (`guest/`), served by the cloud at
   `/qr/`. It collects no personal data and keeps the cart in the browser; it submits through the
   existing intake. Strings go through translation keys (`en`, `vi`, and the store's languages).
3. **Payment for a QR order** stays with staff at the table or counter; QR payment by gateway
   remains ADR-0155's, outside phase 6.
4. **A kiosk** is a till on `DEVICE_CLASS_KIOSK` signed in as a **kiosk staff record** the cloud
   creates per kiosk device, holding a role `KIOSK` whose only permissions are opening an order,
   adding and removing its lines, and sending it to be paid. It has no PIN and no drawer, and
   leaving the kiosk screen needs a manager's PIN (ADR-0201). The till signs it in when it starts.
5. **A kiosk order** is a tableless order on its channel (eat in or take away) with a queue number;
   it is paid at the counter (pay first, ADR-0222), and a card at the kiosk waits for the card
   terminal (phase 8). An order abandoned for 120 seconds is voided with a published reason.
6. **Catalogue:** the self-order template's screens use `kiosk_menu`, `kiosk_cart` and
   `kiosk_done` components and the existing acts.

## Configuration

- **The capability** `qr_ordering_enabled` and the existing QR guardrails node (business hours, rate
  limits, staff confirmation, ADR-0057, ADR-0116), which the self-order preset turns on.
- **A device's class:** a till is a kiosk where the console sets `DEVICE_CLASS_KIOSK` (ADR-0201).
- **A setting** (ADR-0160): `kiosk.abandon_seconds`, 30 to 900, **default 120**: how long an
  untouched kiosk order waits before it is voided.
- **Which channels a kiosk offers** (eat in, take away) are the store's accepted channels.
- **The self-order template's screens** for the guest page and the kiosk are its package's.
- **Fixed rules,** all security: the guest page collects nothing personal; the `KIOSK` role's
  permissions are a fixed set a tenant cannot widen; leaving the kiosk needs a manager's PIN.

## Consequences accepted

- A new public read route, a new front end, a new role and a staff record per kiosk device.
- The guest page is a new internet-facing surface: phase 9's security review covers it.
