# ADR-0190 — A screen is drawn from the package

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Amends
[ADR-0109](0109-counting-the-taps-an-operator-makes.md) (where the tap budget is checked) · Relates
to [ADR-0020](0020-i18n-runtime.md), [ADR-0043](0043-translation-grid.md),
[ADR-0066](0066-cloud-catalog.md), [ADR-0177](0177-openapi-is-the-route-map.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

HQ is to design the till's screens and buttons in the console. Today every screen is hand-written in
Solid (`ui/src/screens/*.tsx`), listed in one table (`ui/src/state/screens.ts`). Only the button grid
is data: the `layout` node's `DisplayPlan` per sales channel ([ADR-0066](0066-cloud-catalog.md)).

Three gates hold the hand-written screens, and a screen built from data would escape them:

- **Taps** ([ADR-0109](0109-counting-the-taps-an-operator-makes.md), `docs/ui-ux.md` §1 principle 6):
  a common act at most two taps from the role's home screen, a rare one at most three. It is checked
  against the source (`ui/scripts/step-budget.mjs`) and replayed in a browser.
- **Device classes** (`ui/scripts/device-classes.mjs`): a screen names the hardware it adapts to
  (phone, tablet, terminal, kitchen display), never a pixel width.
- **Strings**: every user-visible string is a translation key (AGENTS.md §2).

Server-driven UI elsewhere (Airbnb, Zalando, Spotify) teaches the same lessons: a typed contract, a
minimum client version per component, a fallback for an unknown component, validation on both
sides, and last-known-good.

## Options considered

1. **Ship arbitrary HTML or JavaScript in the package.** Unbounded paint cost, no tap budget, and
   code from the console running on every till.
2. **Theme and hide on hand-written screens.** Too little for the six business types.
3. **A tree of typed components from a catalogue the release declares, rendered by one renderer**
   (chosen).

## Decision

1. **A screen is a tree of components.** A package screen has a `screen_id`, the role whose home it
   may be, and a tree whose nodes each name a component from the catalogue and give its typed
   properties. Actions are bound by catalogue id ([ADR-0191](0191-every-act-is-in-a-typed-catalogue.md)),
   never by code.
2. **The catalogue is the release's.** Each component is declared once in `pos-proto`: its id, its
   typed properties, the device classes it supports, the release that introduced it, and a
   `fallback_component`. The console's types and the till's come from it through the existing
   generation (ADR-0177). The first catalogue covers what today's screens use: the button grid,
   order lines, totals, keypad, tabs, list, text, image, step indicator, and form fields for custom
   data (ADR-0194). A new component is additive; a published one is deprecated, never removed.
3. **One renderer in the till.** It walks the tree and mounts Solid components from the catalogue.
   A screen is at most 400 nodes and 12 levels deep, refused at publish beyond that, so its render
   cost is bounded and ADR-0197's budget can hold.
4. **An unknown component falls back.** The minimum edge version (ADR-0189) should prevent it. If
   one still arrives, the renderer draws its `fallback_component`, or an empty slot that says the
   till needs an update, and draws the rest of the screen. It never fails the whole screen.
5. **Device class is a dimension.** A screen may carry variants per device class
   ([ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md) decision 4). A component used on a
   class it does not support is refused at publish.
6. **Labels are translation keys.** A package holds no user-visible text. Every label is a key in
   the tenant's translation grid ([ADR-0043](0043-translation-grid.md)), with `en` required; a key
   without an `en` value is refused at publish.
7. **The tap budget is checked at publish.** Each catalogue action states whether it is common or
   rare (ADR-0191). The cloud computes the fewest taps from each role's home screen to every action
   the version offers that role, over the screen graph the version declares, and refuses a version
   in which a common act takes more than two taps or a rare one more than three. The browser replay
   of ADR-0109 keeps running against the default package.
8. **Validated twice.** The cloud validates a version before publish; the edge validates again when
   it applies it, with last-known-good (ADR-0189 decision 6).

## Consequences accepted

- **Additions to `pos-proto`:** the component catalogue and screen types, additive, with the
  owner's review; a catalogue snapshot under `docs/snapshots/` so a component cannot vanish.
- **Today's screens become the default package** in phase 2, one by one, each passing ADR-0109's
  replay and ADR-0197's budget before the hand-written one is removed.
- **What the catalogue cannot express cannot be built in the console.** A new component is a release.
  That is the price of a bounded paint cost and a checked tap budget.
- **Two validators**, cloud and edge, share the Rust types, so they cannot disagree on shape.
