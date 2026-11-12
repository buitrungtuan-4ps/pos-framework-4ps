# ADR-0210 — A role names its home screen in the package, and taps are counted from it

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-12
· Amends [ADR-0190](0190-a-screen-is-drawn-from-the-package.md) decisions 1 and 7 (decision 7
  now warns instead of refusing)
· Relates to [ADR-0109](0109-counting-the-taps-an-operator-makes.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)

The owner chose option 2 on 2026-11-12, with an act over its tap budget a warning, never a refusal
(decision 5).

## Context

ADR-0190 decision 7 has the cloud refuse a version in which a common act is more than two taps, or a
rare act more than three, from **the role's home screen**. Decision 1 says a package screen carries
"the role whose home it may be". Each catalogue act already has its `ActionFrequency`
(`pos_proto::actions`).

Nothing defines a home screen today:

- `pos_proto::screen::Screen` has only `screen_id` and `root`. No field names a role or a home.
- `VariantSelector.role_id` picks *which variant* of a screen a role sees. It does not say which
  screen the role starts on.
- The till's hand-written screens start where `ui/src/App.tsx` routes after sign-in, and ADR-0109's
  declarations (`ui/scripts/step-tasks.mjs`) count from there. That start is code, not package data.

Without a home, decision 7 has no start to count from. So the check cannot be built without a
decision on the wire, and a `pos-proto` change needs an ADR first (AGENTS.md §2).

Counting taps also needs rules that decision 7 leaves open: what counts as one tap, whether flows
count, and what happens to a screen the version does not declare.

## Options considered

1. **A field on the screen**, `home_role_ids: Vec<Ulid>` (empty means no role), plus `home: bool` for
   the base home. This follows decision 1's wording. But a home is chosen per role, and a
   screen's variants are chosen per role too, so two places would answer role questions. Checking
   that each role has exactly one home also means scanning every screen.
2. **A list on the version**, `PackageContent.home_screens: Vec<Variant<HomeScreen>>`, where
   `HomeScreen { screen_id }` and the selector states at most `role_id` (and optionally device
   class). The base variant is the home for every role without its own. This reuses the variant
   rule (`pos_core::package::resolve`, `check_variants`), so "exactly one home per role" is already
   proven. It is additive, and a version without it runs as today.
3. **A convention**: the screen whose id is `home` is the home, and its role variants are the
   per-role homes. No wire change, but the rule lives in nobody's type and a rename breaks it.
4. **No package home**: count from every screen the version declares. This is stricter than
   ADR-0109 and refuses designs that are fine, such as a manager-only settings screen deep in the
   graph.

## Decision (option 2)

1. **A version names its homes.** `PackageContent` gains `home_screens`, a variant list of
   `HomeScreen { screen_id }`, left off the wire while it is empty. Its selectors may state only
   `role_id` and `device_class`. It needs a base variant whenever the version declares a screen, and
   every `screen_id` it names must be a screen the version declares. A version with no screens has
   no homes and nothing to count. The till opens the resolved home after sign-in, and the edge
   checks the list on apply as it does the other parts.
2. **The graph.** For each role the version states, plus "any other role" (the base), and for each
   device class: resolve every screen and the home for that role and class. A node is a screen. An
   edge is a node property of kind `ACTION` whose act is `ui.screen.open` with a fixed `screen_id`,
   and it costs one tap. Any other action property on a screen offers that act, at the screen's
   distance plus one.
3. **Flows** ([ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)). A flow is entered by its
   `start_action_id`. An act on one of its steps' screens costs what the start act costs, plus that
   screen's distance from the step. A step's `prerequisites` add nothing: they constrain order, not
   taps.
4. **Screens the version does not declare** are drawn from the release's default package, which
   ADR-0109's replay already holds. They end the graph: acts on them are not counted here.
5. **What is warned, never refused.** An act the role can reach that is `COMMON` and costs more
   than two taps, or `RARE` and costs more than three, is a warning, reason `TAP_BUDGET`. The
   version still publishes. The warning names the role (its id, or base), the device class, the
   act, the screen and the node that offers it at the cheapest cost, and the cost. The designer
   shows it beside that node, the dry run lists it with `screen_index` and `node_index`, and the
   published version keeps the count of its warnings, which the version comparison shows beside
   the previous version's. An act the role cannot reach at all is not offered to it, and is not
   warned. The budgets stay two and three; making them a setting is left for when a tenant needs it.
6. **Order.** The check runs after the content, flow, reaction and key checks, and before the
   evolution check.

## Consequences accepted

- An additive `pos-proto` field and a new `min_edge_version` floor for a version that states
  homes, with the owner's review of `pos-proto`.
- The till's post-sign-in route depends on package data for a version that states homes, and the
  hand-written route stays for the default package.
- Permissions are not part of the graph: an act the role may not perform still counts wherever it
  is offered. That warns more often, never less, and keeps the count simple.
- ADR-0190 decision 7 no longer refuses a version. A slow design can be published, and is visible
  only through its warnings. The browser replay of ADR-0109 keeps running against the default
  package, where ADR-0208's gate still blocks a slow till.
