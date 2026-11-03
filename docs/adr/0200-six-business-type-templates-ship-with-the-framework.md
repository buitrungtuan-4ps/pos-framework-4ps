# ADR-0200 — Six business-type templates ship with the framework

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Relates to
[ADR-0027](0027-country-modules.md), [ADR-0105](0105-a-country-pack-is-values.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner wants six business types ready to use: a full-service chain restaurant, a café or bubble-tea
shop, fast food, a bar with tabs, a bakery or retail shop (barcode, scale), and self-order (QR,
kiosk).

What exists:

- **Three capability presets** in `pos-core` (`capability.rs`): full-service, café or counter, and
  retail, each tested to satisfy every inter-flag rule. Roadmap-v3 D12: one order and bill machine,
  formats differ only by preset.
- **Country packs are values** ([ADR-0105](0105-a-country-pack-is-values.md)) in `countries/<cc>/`,
  and seed a new store's settings ([ADR-0183](0183-a-country-pack-seeds-a-new-stores-settings.md)):
  the country pack first, then the register's global preset.
- **Packages** ([ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md)) will carry screens,
  flows, rules and data, with today's screens as the default package.

## Options considered

1. **One preset per business type in code, as today.** Screens and flows would stay hand-written per
   type, which the package exists to end.
2. **Templates as console-only examples.** Untested, and they would drift from the release.
3. **Templates as packages shipped and tested with the framework, like country packs** (chosen).

## Decision

1. **A template is a package plus values**, in `business-types/<id>/` in the repository:
   - a package (screens, flows, rules, data) built only from the release's catalogues;
   - a capability preset, extending today's three to six;
   - recommended settings for a new store, as *(setting key, value)* pairs checked against the
     register;
   - an obviously fake demo menu and floor, for the simulator and the console's preview.
2. **Six ids:** `BUSINESS_TYPE_FULL_SERVICE`, `BUSINESS_TYPE_CAFE`, `BUSINESS_TYPE_FAST_FOOD`,
   `BUSINESS_TYPE_BAR`, `BUSINESS_TYPE_BAKERY_RETAIL`, `BUSINESS_TYPE_SELF_ORDER`, with
   `BUSINESS_TYPE_UNSPECIFIED` meaning none.
3. **Choosing a template copies it.** When HQ creates a brand or a store from a template, the tenant
   receives the template's package as its own package, version 1, and pins it
   ([ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md) decision 2). From then on it is the
   tenant's to change. A later release's template is offered beside it and never applied by itself.
4. **Seeding order.** A new store's settings resolve as: the country pack, then the template, then
   the register's global preset, writing only what the store does not set (ADR-0183's rule, with the
   template inserted). A test refuses a template that sets a key any country pack sets, so the two
   never compete: the country states law and money, the template a way of working.
5. **Each template needs what it uses.** Bar tabs, weighed items, self-order, barcode and PLU, the
   pre-bill event, comp and price override, and split-bill partial payments visible to other tills
   each arrive in phase 6 with their own ADR. A template ships only once everything it uses is in
   the catalogues.
6. **A template-by-country matrix.** Every template is run against every shipped country pack (`vn`,
   `jp`, `in`, and the neutral `zz`) in the simulator: open, order, pay, and close a shift. A pull
   request that touches a template or a country pack runs its row or column; the nightly job runs all
   of it.

## Consequences accepted

- **Additions to `pos-proto`:** the business type enum, additive; the capability presets grow from
  three to six in `pos-core`, each tested as today's are.
- **A new top-level directory**, `business-types/`, entered in `docs/repository-map.md` when the
  first template lands.
- **24 matrix cells to keep green.** They are what makes "plug and play by business type" true.
- **A tenant's copy drifts from later templates.** That is the intent: HQ owns its package.
