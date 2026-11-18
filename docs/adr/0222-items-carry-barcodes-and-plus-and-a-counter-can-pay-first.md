# ADR-0222 — Items carry barcodes and PLUs, a quick sale scans them, and a counter can pay first

**Status** Accepted 2026-11-18 (owner) · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Completes roadmap-v3 B5.1 and B5.2 · Needed by
[ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md) decision 5 · Amends
[ADR-0146](0146-a-counter-store-starts-its-own-orders.md) ("pay-first stays out") · Relates to
[ADR-0105](0105-a-country-pack-is-values.md), [ADR-0161](0161-a-paid-order-without-a-table-stays-on-the-kitchen-board-until-it-is-done.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Accepted by the owner on 2026-11-18 as recommended: an open-price item typed within its published
bounds needs no manager; on a pay-first store the settle fires the unsent lines; phase 6 supports
keyboard-wedge scanners only, and camera scanning comes with the devices in phase 8.

## Context

A bakery or shop sells by scanning. `barcode_enabled` exists and nothing reads it; a menu item has
no barcode or PLU. The counter flow is add, send, bill, pay (ADR-0146), and `pay_first_enabled` has
no reader because settling before sending once hid the ticket from the kitchen; ADR-0161 has since
kept a paid tableless order on the board. Café, fast food, bakery and kiosk all pay first.

## Options considered

1. **Look codes up on the edge per scan.** A round trip per scan against a 10 ms paint budget.
2. **A scanner driver on the edge.** Most scanners are keyboards; a driver buys nothing now.
3. **Codes on the item, indexed in the till, typed in by a keyboard-wedge scanner** (chosen).

## Decision

1. **Codes on the item** (additive on the catalogue, `MenuEntry` and the cloud's catalogue store):
   `barcodes`, up to 8 GTIN-8, -12, -13 or -14 with a valid check digit, and `plu`, 1 to 5 digits.
   Each is unique within a menu; the console refuses a duplicate, naming both items.
2. **The till indexes them** when the menu arrives and resolves a scan without a round trip. The
   edge still prices the line (ADR-0146). A scanner is a keyboard: on a screen that listens, digits
   ending in Enter within 50 ms are a scan. Camera scanning on phones waits for phase 8.
3. **Variable-measure codes** (GS1 prefixes 20 to 29, a price or a weight inside) are read by one
   pure `pos-core` function from a layout the **country pack** declares, because each national GS1
   body assigns them. A pack that declares none reads them as plain codes.
4. **Open-price items.** An item may be `open_price` with a published minimum and maximum; the
   cashier types the price within them with `sales.line.add` alone. Outside them is a price
   override (ADR-0219).
5. **Quick sale** is a package screen, not new machinery: a counter order, the code field, a PLU
   keypad, search, the cart and pay. Catalogue: `sales.line.scan` (`ACTION_EFFECT_EDGE_WRITE`) and
   the components `scan_field` and `plu_keypad`.
6. **Pay first.** On a store with `pay_first_enabled`, settling a tableless order fires its unsent
   lines in the settle's own transaction, so the kitchen sees the order once it is paid (ADR-0161
   keeps it there). `tables_enabled` with `pay_first_enabled` stays refused.
7. **Capability readers:** `barcode_enabled` turns on the scan listener and the code fields;
   `pay_first_enabled` the fire at settle.

## Configuration

- **Capabilities** (§10): `barcode_enabled`, which the bakery-retail preset turns on, and
  `pay_first_enabled`, which the café, fast-food and bakery-retail presets turn on.
- **Catalogue data,** per item: its barcodes, its PLU, and whether it is open-price with its minimum
  and maximum, authored in the console.
- **A setting** (ADR-0160): `scan.keystroke_window_ms`, 20 to 200, **default 50**: how quickly a
  scanner's digits arrive, which tells a scan from typing on a given model of scanner.
- **Variable-measure layouts** are a setting each country pack seeds for a new store (ADR-0183),
  because each national GS1 body assigns its prefixes; a store may override it.
- **Fixed rules:** a GTIN's check digit and a code's uniqueness within a menu (data integrity); the
  edge prices every line (money integrity); pay-first with tables stays refused (§10's rule).

## Consequences accepted

- Additive catalogue fields, a migration on each store adapter, a country-pack value, two acts and
  two components; no new event (the fire at settle writes the existing `sales.order_line.fired`).
- A code index in the till, bounded by the menu (at most 20,000 codes).
