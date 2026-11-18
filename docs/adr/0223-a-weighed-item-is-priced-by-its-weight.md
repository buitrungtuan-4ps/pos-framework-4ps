# ADR-0223 — A weighed item is priced by its weight, read from a label, a scale or a hand

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Needed by [ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md) decision 5 ·
Adds a port (AGENTS.md §7) · Relates to [ADR-0021](0021-corrected-port-list.md),
[ADR-0105](0105-a-country-pack-is-values.md), [ADR-0153](0153-a-vendor-is-a-provider-the-cloud-chooses.md),
ADR-0222

Waits on the owner: whether a real scale driver ships now or in phase 8, and whether a weight may be
typed by hand.

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
   - a **hand-typed** weight, only where the country pack's `weight_manual_entry` value allows it.
3. **The country pack states the law**: `weight_manual_entry` (allowed or not) and the smallest
   weight a line may carry. `zz` allows hand entry; `vn`, `jp` and `in` are set by phase 7's legal
   suites, refusing hand entry until then.
4. **Catalogue:** `sales.line.weigh` (`ACTION_EFFECT_EDGE_WRITE`, waits on an external device and is
   excluded from the 10 ms budget for that wait, ADR-0197) and a `weight_display` component.

## Consequences accepted

- A new port, an additive event field, additive catalogue fields and two country-pack values.
- A real scale driver (a vendor's protocol) is an adapter, added in phase 8 with the other devices.
