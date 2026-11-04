# ADR-0203 — A component reads its own data, and a package gives it only literals

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03
· Amends [ADR-0190](0190-a-screen-is-drawn-from-the-package.md) decisions 1 and 2
· Relates to [ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md),
[ADR-0197](0197-every-tap-paints-within-ten-milliseconds.md)

Accepted by the owner on 2026-11-04, who asked for the freest model that still keeps ADR-0197's
budget and chose to reach it in two steps: this record now, and compiled bindings in phase 4
(decision 5).

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
5. **Step two: compiled bindings in phase 4.** With the console designers, option 2 is added as a
   new property kind under its own ADR, so HQ can wire data into a component without a release.
   It keeps the budget by these rules, which that ADR states in full:
   - **Typed scope.** Each screen declares the schema of the data it offers. A path that does not
     resolve, or whose type does not match the property, is refused at publish.
   - **Compiled at publish.** The cloud compiles every path into a direct accessor. The till runs
     the compiled form and never parses a path while serving.
   - **Bounded repetition.** Every `each` declares a maximum and draws only the rows on view.
   - **Fine-grained updates.** A binding re-renders only when the data on its path changes. A
     `when` guard is re-evaluated on a change to what it reads, never on every frame.
   - **A cost budget at publish.** The cloud computes each screen's cost (nodes, bindings, bounds)
     and refuses a version that exceeds ADR-0197's budget, naming the screen and the part over.
   - **Measured in the field.** Till telemetry reports p95 per screen and per package version.

   This record's components stay valid beside bindings, and every package written under it keeps
   drawing unchanged.

## Consequences accepted

- **A new source of data is a new component, and a new component is a release.** ADR-0190 already
  accepts that what the catalogue cannot express cannot be built in the console.
- **The paint cost is bounded per component**, which keeps ADR-0197's budget checkable.
- **Today's screens can be expressed as the default package** with this catalogue, which is
  phase 2's exit criterion.
- **HQ customises layout, labels, choices and acts, not data plumbing**, until phase 4's record
  adds compiled bindings.
- **Step two costs more than this record.** It needs a per-screen schema, a path compiler, bounded
  lists and a publish-time cost model, built with the phase-4 designers so HQ picks data rather than
  typing paths.
