# Templates at the end of phase 6, November 2026

**Status** Accepted · **Owner** @maintainers-architecture · **Measured** 2026-11-19

The exit record of roadmap-v4 phase 6 (templates): what the exit criterion asked, what shows it is
met, and what the phase's scope still leaves open. Like
[gateway-functions-2026-11-phase-5-exit.md](gateway-functions-2026-11-phase-5-exit.md), it records
one moment and is not regenerated.

**Measured at** `a0b9ad4d` on `main`.

## The criterion

**Each template sells end to end in the simulator for each of the three countries**
([ADR-0200](../adr/0200-six-business-type-templates-ship-with-the-framework.md)). The six business
types (full service, cafe, fast food, bar, bakery and retail, self-order) ship as templates with
their presets (#1301), and the simulator's matrix sells each in Vietnam, Japan, India and the
neutral `zz` pack through an in-process edge (#1302), nightly and on the row or column a pull request
touches (#1303).

## The criterion is met

`POS_EDGE_RELEASE_VERSION=0.14.1 cargo run -p pos-simulator -- matrix` passes all 24 cells at
`a0b9ad4d`: every template takes an order, prints its receipt and balances the drawer in `vn`, `jp`,
`in` and `zz` (for example `self-order vn receipt 1 paid 260000 VND drawer 260000`). It runs in
every gate of the phase, beside the till replay (242 flows at the end, none unexpected, normally and on two
CPUs).

## What each ADR shipped

Every ADR was accepted by the owner on 2026-11-18 with a Configuration section: business
preferences are published settings (ADR-0160) with defaults and template presets; only legal,
money-integrity and security rules are fixed.

| ADR | What shipped | Pull requests |
|---|---|---|
| [0218](../adr/0218-a-printed-check-is-an-event-every-till-sees.md) the pre-bill | A printed check is an event every till sees; the till shows it and when it is out of date | #1294, #1305, #1307 |
| [0219](../adr/0219-a-comp-and-a-price-override-each-need-a-reason-and-an-approver.md) comp and price override | A comp gives a whole line away, a price override changes a line before its bill opens within a published bound, each with a reason and an approver; the till's panels | #1295, #1306, #1308, #1313 |
| [0220](../adr/0220-a-bill-takes-its-payments-one-at-a-time.md) part payments | A bill takes its payments one at a time and the last settles it; voiding a part-paid bill gives each back; every till sees what is paid | #1296, #1309, #1311, #1312, #1315 |
| [0221](../adr/0221-a-bar-tab-is-an-order-without-a-table-that-stays-open.md) bar tabs | A tab opens with the day's number, every till lists them, an empty one closes with a reason, an optional guest label kept only in the store; the Tabs screen and its replay; the console offers tabs | #1297, #1320, #1323, #1324, #1325, #1327 |
| [0222](../adr/0222-items-carry-barcodes-and-plus-and-a-counter-can-pay-first.md) barcodes, PLU, quick sale, pay-first | Items carry barcodes, a PLU and an open-price range (console and cloud); the till reads a scan within a published keystroke window; open-price items; variable-measure labels read by the country's layout; the code field and PLU pad; a pay-first counter fires on settle and takes payment without asking to send | #1298, #1310, #1314, #1317, #1318, #1319, #1321, #1322, #1326, #1328 |
| [0223](../adr/0223-a-weighed-item-is-priced-by-its-weight.md) weighed items | The `Scale` port; a weight label sells a weighed item; the scale or a typed weight under the country's law (`weighing.manual_entry`, refused by the cloud where the country forbids it); the till's weight display; the console's weighed-item fields | #1299, #1316, #1329, #1330, #1331, #1332 |
| [0224](../adr/0224-self-order-is-a-guest-page-and-a-kiosk.md) self-order | The guest's menu (`GET /v1/qr/menu`); the guest page at `/qr/`, which asks nothing personal and runs under its own content security policy, and bounds on a guest's order; a kiosk that signs in as the record the store derives from its terminal's class, holds the fixed `KIOSK` set, reaches only its own orders and not the event stream, and is left only by someone holding `admin.kiosk.exit` with their PIN; the kiosk's till, with `kiosk.abandon_seconds` and the `ABANDONED` reason | #1300, #1333, #1334, #1335, #1336, #1337 |

## Decisions taken during the phase

Each within its accepted ADR, recorded here because the ADR text did not settle it.

- **Manual weighing is checked twice, as law** (ADR-0223 decision 3). A country pack names the
  settings it forbids; the cloud refuses `weighing.manual_entry = true` for a store whose country
  forbids it (`SETTING_FORBIDDEN_BY_COUNTRY`, #1332), and the edge refuses a typed weight there
  (`MANUAL_WEIGHT_NOT_ALLOWED`, #1330), so neither side trusts the other alone.
- **A kiosk's record is derived on the edge** (ADR-0224 decision 4, #1335). Decision 4's "staff record
  the cloud creates per kiosk device" is met by the terminal's class: the cloud publishes it in the
  signed `devices` node, and the store derives the record from that node alone, with the terminal's
  own id, a code that never signs in and no PIN. No field was added to `pos-proto`; the `KIOSK` role is
  the code constant `KIOSK_GRANTS` (`sales.line.add`, `billing.bill.open`), not a role row, and the
  edge holds the record to it whatever is published. What a kiosk wrote keeps its terminal's id after
  the terminal is retired (#1336).
- **Leaving a kiosk is a permission of its own**, `admin.kiosk.exit` (PIN-flagged, manager and owner
  by default, granted to existing roles on `admin.device.manage`'s terms by migration 0104), so a
  tenant can let a shift lead take a kiosk back without managing devices (#1335).
- **An abandoned kiosk order cites `ABANDONED` or nothing** (ADR-0224 decision 5). The framework's
  reason list gains `ABANDONED` (void a line, close an order). Where a store's list lacks it, the kiosk
  never cites another reason in its place: it leaves the order open for staff and starts over. An
  order sent to be paid is never touched (#1335, #1337).
- **A kiosk does not open `/ws`**, the store's event stream, which carries every till's orders, and
  reaches only self-order's routes on orders it opened (`KIOSK_NOT_ALLOWED`, #1336).
- **A guest's order is bounded** (#1334): 1 to 30 lines, 1 to 99 whole items a line, at most 16
  modifiers, and no note, free text a guest could put personal data in.

## Built beside the criterion

- **The template matrix** (#1301, #1302, #1303): six business types with their presets and template
  directories, every template sold in every country through an in-process edge, run nightly and on
  the row or column a pull request touches.
- **A pay step that meets "send it first?"** answers Send and pay (#1304).
- **Replays per new till feature**: the check, comp and price override, part payments, tabs, quick
  sale, weighing, the guest page and the kiosk each have their own, and the whole replay grew from
  230 to 242 flows, every one passing normally and on two CPUs.

## What the phase's scope still leaves open

- **GS1 variable-measure practice for `vn`, `jp` and `in`** (phase 7). Each national GS1 body
  assigns the prefixes 20 to 29; until each is confirmed those packs seed no
  `scan.variable_measure_layout`, and only `zz` seeds `PRICE_5` (ADR-0222 decision 3).
- **Manual weighing for `vn`, `jp` and `in`** (phase 7's legal suites). `weighing.manual_entry` is off
  by default and the cloud refuses turning it on where a country pack forbids it; which of the three
  forbid it, and whether a certified scale is required, is for each country's legal acceptance suite
  (ADR-0223 decision 3).
- **Comp tax per country** (phase 7): the accountant confirms how each country taxes a comped line
  (ADR-0219).
- **No alcohol rate yet for India and `zz`.** The bar template sells there on the standard rate; a
  rate of their own waits on the country work.
- **Card on a tab, camera scanning and real scale drivers** (phase 8, devices). A tab is paid at the
  counter like any bill; a barcode is read by a keyboard-wedge scanner; the `Scale` port has a
  contract suite and a fake, and no hardware driver.
- **Typed open price on table orders** is not supported: an open-price item is sold on a counter
  order, and a table line keeps the device's copied price (ADR-0222 decision 4).
- **The guest page's security review** (phase 9). `/qr/` and the guest routes are a new internet-facing
  surface; the page is bounded and runs under its own content security policy, and its review is
  phase 9's (ADR-0224, Consequences).
- **The PDPD lawful basis and a DPIA** are owed before any store turns on a tab's guest label
  (`tabs.guest_label`): the label may carry a guest's name, kept only in the store and off by default
  (ADR-0221).
- **A kiosk takes no card.** A kiosk order is paid at the counter; a card at the kiosk waits for the
  card terminal (phase 8, ADR-0224 decision 5).
- **The console's QR sheet** lists each table's signed token; printing it as the guest page's
  address, `/qr/#t=<token>`, as a QR code is open.
