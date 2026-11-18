# ADR-0223 — A weighed item is priced by its weight, read from a label, a scale or a hand

**Status** Accepted 2026-11-18 (owner) · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Needed by [ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md) decision 5 ·
Adds a port (AGENTS.md §7) · Relates to [ADR-0021](0021-corrected-port-list.md),
[ADR-0105](0105-a-country-pack-is-values.md), [ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
ADR-0222

Accepted by the owner on 2026-11-18 as recommended: the `Scale` port, its contract tests and a fake
come in phase 6 and real drivers in phase 8; a weight typed by hand is allowed only in `zz` until
phase 7, as a setting the country pack seeds; a weighed item is priced per kilogram and its line
quantity is in grams.

## Context

Bread, cheese, salad bars and tea leaves are sold by the kilogram. A quantity is already counted in
thousandths (`pos_proto::quantity`), so 0.375 kg is `375` and `Quantity` extends a price with
explicit rounding, with no float. What is missing is an item that says it is sold by weight, a
source for the weight, and the law: in many countries only a certified scale may set a weight sold
by it (legal for trade).

## Options considered

1. **Typed weights only.** Fast to ship, illegal where trade law requires a scale.
2. **A scale read by the till in the browser.** A browser cannot reach a serial or USB scale on every
   device class; the edge can.
3. **Three sources behind one rule, a scale port on the edge, the law in the country pack** (chosen).

## Decision

1. **Sold by weight.** An item gains `sold_by`, `SOLD_BY_UNIT` (the default) or `SOLD_BY_WEIGHT`,
   additive. A weighed item's price is per kilogram and its line quantity is the net weight in
   grams (thousandths of a kilogram); the total is price times weight, rounded once by the
   currency's rule. An optional `tare_grams` is taken off a scale's reading.
2. **Three sources**, recorded on the line as an additive `weight_source` on
   `sales.order_line.added` (`WEIGHT_SOURCE_LABEL`, `_SCALE`, `_MANUAL`):
   - a **label** printed by a labelling scale, read as a variable-measure code (ADR-0222 decision 3);
   - a **scale** beside the till, through a new port, `Scale` (the 23rd): read one stable net weight
     with its unit, or answer unstable, overloaded or absent. The edge drives it (a device on the
     store's network or a port, ADR-0153). Phase 6 ships the port, its contract tests and a fake;
   - a **hand-typed** weight, only where the store's `weighing.manual_entry` setting, seeded by its
     country pack, allows it.
3. **The country pack states the law**: it seeds `weighing.manual_entry`. `zz` allows hand entry;
   `vn`, `jp` and `in` refuse it until phase 7's legal suites decide.
4. **Catalogue:** `sales.line.weigh` (`ACTION_EFFECT_EDGE_WRITE`, waits on an external device and is
   excluded from the 10 ms budget for that wait, ADR-0197) and a `weight_display` component.

## Configuration

- **Catalogue data,** per item: `sold_by` and `tare_grams`, authored in the console.
- **A setting** (ADR-0160), seeded by the country pack (ADR-0183): `weighing.manual_entry`, on or
  off, **default off**; the `zz` pack seeds it on, and `vn`, `jp` and `in` seed nothing until
  phase 7's legal suites decide. A store may set it only where its country allows (the cloud refuses
  a value its pack forbids, as law).
- **A setting:** `weighing.min_grams`, 1 to 1,000, **default 1**: the smallest weight a line may carry.
- **The bakery-retail template** uses weighed items in its demo menu.
- **Fixed rules:** kilograms only, a line in grams and one rounding by the currency's rule (money
  integrity); the weight's source on every line (audit); a scale's reading is never edited.

## Consequences accepted

- A new port, an additive event field, additive catalogue fields and two settings, one seeded by
  the country pack.
- A real scale driver (a vendor's protocol) is an adapter, added in phase 8 with the other devices.
