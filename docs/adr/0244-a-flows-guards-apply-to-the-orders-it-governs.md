# ADR-0244 — A flow's guards apply to the orders it governs, as the flow says

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (open item O1 of the phase 10 report) · **Amends decision 4 of
[ADR-0192](0192-a-flow-is-a-graph-of-steps-the-edge-re-checks.md)** · Relates to
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0189](0189-a-store-runs-a-package-version-hq-pins.md),
[ADR-0191](0191-every-act-is-in-a-typed-catalogue.md)

## Context

ADR-0192 decision 4 has the edge re-check a flow's guards before every command for a guarded act.
It never says *which orders* a flow governs, so the edge checks every order that takes the act. In
the phase 10 demo a dine-in flow that asks the guest count before the first line refused every
counter, takeaway and delivery order too, because each of them adds a line. Nothing records that an
order "entered" a flow.

## Options considered

1. **Only orders that entered the flow, by its start act.**
2. **Orders on chosen channels.**
3. **Every order**, as today.
4. **Warn in the console only.**

The owner chose all three as a per-flow choice, with option 1 as the default and option 4 on top.

## Decision

1. **Each flow says which orders its guards apply to**, a field of the flow in the package
   (`pos-proto`, additive), so it is authored per package version and reaches a tenant, store group or
   store through the package pins of ADR-0189:
   - `guard_scope`: `FLOW_GUARD_SCOPE_ENTERED` (the default; absent reads as it),
     `FLOW_GUARD_SCOPE_CHANNELS` or `FLOW_GUARD_SCOPE_ALL_ORDERS`;
   - `guard_channels`: the sales channels, one or more, that `CHANNELS` applies to.
2. **"Entered" is recorded, not guessed.** `sales.order.opened` gains an optional, additive
   `opened_by_action_id`: the catalogue act whose route opened the order (`sales.table.seat`,
   `sales.order.open`, `sales.tab.open`). An order opened by intake (QR, delivery, the public API)
   records none. An order entered a flow when the act that opened it is the flow's
   `start_action_id`. An order opened before this field entered no flow.
3. **The edge re-check follows the same scope.** Before a guarded command the edge evaluates only
   the flows whose scope includes the order. The till's disabled-act check uses the same rule, from
   the same fields, so the two answers agree.
4. **A start act that does not open an order** (a lookup, a line) cannot be recorded on the order.
   Under `ENTERED`, such a flow's guards guide the till and are not enforced by the edge, and the
   console says so.
5. **The console Check warns** when a guard can block orders outside the flow: a flow with an edge
   guard under `ALL_ORDERS`, or under `CHANNELS` (orders on those channels that did not start the
   flow are checked too), and a flow under `ENTERED` whose start act opens no order. A warning does
   not stop a publish.

## Configuration

- **Per flow, per package version:** `guard_scope` and `guard_channels`. Default `ENTERED`.
- **Fixed:** the edge never enforces a guard on an order outside the flow's scope, and never skips one
  inside it.

## Consequences accepted

- **Behaviour changes on upgrade**: a published flow that blocked orders outside it stops doing so,
  which is the defect this corrects. A package that relied on it sets `ALL_ORDERS` or `CHANNELS`. An
  order open across the upgrade carries no opening act and is no longer guarded.
- An additive field on a flow and on one event, both checked against `PROTOCOL_VERSION` as `pos-proto`
  requires.
