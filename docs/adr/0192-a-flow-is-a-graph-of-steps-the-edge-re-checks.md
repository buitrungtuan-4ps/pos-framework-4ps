# ADR-0192 — A flow is a graph of steps the till runs and the edge re-checks

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-11-03 · Relates to
[ADR-0109](0109-counting-the-taps-an-operator-makes.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0190](0190-a-screen-is-drawn-from-the-package.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md),
[ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md)

Accepted by the owner on 2026-11-03 through approval of the overhaul plan
([`docs/roadmap-v4.md`](../roadmap-v4.md)).

## Context

The owner wants HQ to shape how work goes: required, optional and conditional steps, and "do X before
Y" — ask the guest count before the first line, take a buyer's tax code before an e-invoice bill,
confirm age before alcohol. Today each sequence is code in a screen. The edge enforces only its own
invariants (state machines in `pos-core`, permissions, ceilings); it cannot know that a store wants
a guest count first.

A rule the till alone enforces is a rule a second till, an older till or a direct API call skips. A
rule that changes while an order is open would change the order's rules half-way.

## Options considered

1. **Flows in the till only.** Fast, but unenforced.
2. **A general workflow engine on the edge** (BPMN or a state-chart runtime). A new dependency and a
   second state machine beside `pos-core`'s.
3. **A small step graph as data: the till runs it, the edge checks its prerequisites from facts it
   already holds** (chosen).

## Decision

1. **A flow is a graph of steps.** A package flow has a `flow_id`, the action that starts it, and up
   to 64 steps. Each step has:
   - a `step_id` and what it does: show a screen ([ADR-0190](0190-a-screen-is-drawn-from-the-package.md))
     or run an action ([ADR-0191](0191-every-act-is-in-a-typed-catalogue.md));
   - `STEP_KIND_REQUIRED` or `STEP_KIND_OPTIONAL`;
   - an optional `applies_when`, a CEL condition ([ADR-0193](0193-conditions-and-reactions-are-written-in-cel.md)):
     a conditional step is one that applies only when it holds;
   - `prerequisites`: the steps that must be complete before it can run.
2. **Validated at publish.** Prerequisites form no cycle; every step is reachable; every screen,
   action and condition resolves; a package has at most 32 flows; the tap budget is computed through
   the flows ([ADR-0190](0190-a-screen-is-drawn-from-the-package.md) decision 7).
3. **The till runs it.** It moves the operator through the steps, skips optional ones on a tap, and
   disables an action whose prerequisites are not met, saying which step is missing.
4. **The edge re-checks what matters, from its own facts.** A prerequisite the edge must enforce is
   written as a condition over state the edge holds — the order's guest count, the bill's buyer, a
   line's attribute — never over what the till remembers. Before a command for an action that such
   a guard protects, the edge evaluates it and refuses with `409 FLOW_PREREQUISITE_UNMET`, naming the
   step. A guard that needs state the edge does not hold is refused at publish as an edge guard and
   can only be a till step.
5. **A flow in progress keeps its rules.** An order records the `package_version_id` it opened under,
   as an optional additive field on `sales.order.opened`. Its flows and guards are evaluated under
   that version until the order closes. The edge keeps every version an open order references, at
   most four; an order on a version beyond that is checked under the current one, and that is logged.
6. **Flows never touch money.** A flow step can run a money action only as a person's tap with what
   the person entered (ADR-0191 decision 2). No flow computes an amount.

## Consequences accepted

- **Additions to `pos-proto`:** the flow types, and an optional field on one event, both additive
  (`PROTOCOL_VERSION` checked as `pos-proto` requires), with the owner's review.
- **One new refusal**, `FLOW_PREREQUISITE_UNMET`, with its translation key.
- **Some steps are till-only.** A prerequisite with no edge fact behind it guides the operator and
  cannot be enforced. The console says which kind each step is.
- **The guest count becomes the first flow step** (roadmap-v3 B8.4). It is already a field of
  `sales.order.opened`, so the edge can guard it with no new fact.
