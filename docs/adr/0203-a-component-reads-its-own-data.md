# ADR-0203 — A component reads its own data, and a package gives it only literals

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-03
· Amends [ADR-0190](0190-a-screen-is-drawn-from-the-package.md) decisions 1 and 2
· Relates to [ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Proposed: it waits on the owner's choice among the three options below.

## Context

ADR-0190 makes a screen a tree of catalogue components with typed properties, and binds actions by
catalogue id. It leaves open where a component's data comes from (the order's lines, the menu's
buttons, a bill's totals), what a property may hold, and how a bound action gets the ids it needs
(which `menu_item_id` an item button passes to `sales.line.add`). The three answers decide the
catalogue's shape, so they are decided together. Phase 0's spike (`spike/a-screen-from-a-package`)
used generic `$path` bindings into a scope the host screen supplied, with `each`, `as` and `when`.

## Options considered

1. **A component owns its data** (chosen). Each component reads one fixed, typed part of the
   screen's context; a property is a literal; the component supplies the contextual ids its
   actions need.
2. **Generic bindings, as in the spike.** A property may be a `$path` into a scope the host screen
   provides, with `each` to repeat and `when` to show. Flexible, but it needs a typed scope schema
   per screen to be validated at publish, it is a second small expression language beside CEL, and
   the paint cost is no longer bounded by the node count (an `each` over 300 items).
3. **CEL for bindings.** A property may be a CEL expression over ADR-0193's views, evaluated by
   the till's module. One language, but CEL returns values, not components to repeat, so a
   repetition construct would still be needed, and CEL would run on every paint, not only in guards.

## Decision

1. **A component reads its own data.** Each catalogue component declares the one part of the
   screen's context it shows: `order_lines` the order on screen, `button_grid` the `layout` node's
   buttons for the channel, `totals` the bill, and so on. A package does not say where data comes
   from; it says which component goes where.
2. **A property is a literal.** Its kind is one of `PROPERTY_KIND_TRANSLATION_KEY`,
   `PROPERTY_KIND_TOKEN` (a value from a list the component declares, such as a variant),
   `PROPERTY_KIND_INT` (with the component's bounds, such as a column count), `PROPERTY_KIND_BOOL`
   or `PROPERTY_KIND_ACTION`. There are no paths, expressions or repetition in a package.
3. **An action property is an `action_id` and fixed literal parameters.** The component supplies
   the contextual ids: a button its `menu_item_id`, a line its `order_line_id`, the screen its
   `order_id` or `table_id`. The catalogue declares, per component, which parameters it supplies for
   which actions, and the cloud refuses at publish an action whose required parameters are neither
   supplied by the component nor fixed in the package.
4. **Validation is complete at publish.** Every property is checked against its kind, so a version
   that publishes cannot fail to draw for a reason the cloud could have seen.
5. **Bindings may come later, additively.** If a business-type template (phase 6) needs data a
   component cannot read, options 2 or 3 may be added as a new property kind, under their own ADR.
   Every package written under this record stays valid.

## Consequences accepted

- **A new source of data is a new component, and a new component is a release.** ADR-0190 already
  accepts that what the catalogue cannot express cannot be built in the console.
- **The paint cost is bounded per component**, which keeps ADR-0197's budget checkable.
- **Today's screens can be expressed as the default package** with this catalogue, which is
  phase 2's exit criterion.
- **HQ customises layout, labels, choices and acts, not data plumbing**, until a later record adds
  bindings.
