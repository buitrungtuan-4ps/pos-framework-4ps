# ADR-0219 — A comp and a price override each need a reason and an approver

**Status** Accepted 2026-11-18 (owner) · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Completes roadmap-v3 B2.3 for comp and price override · Needed by
[ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md) decision 5 · Relates to
[ADR-0115](0115-reason-codes-are-a-managed-list.md), [ADR-0146](0146-a-counter-store-starts-its-own-orders.md),
[ADR-0196](0196-every-act-paints-first-and-money-waits-for-the-edge.md)

Accepted by the owner on 2026-11-18 as recommended: a comp lowers the taxable base, which the
accountant confirms per country in phase 7; a comp is a whole line; a price override is allowed only
before the bill opens, above zero and at most a published multiple of the price (100 unless a store
says otherwise), and is recorded as `sales.order_line.repriced`.

## Context

A **comp** gives an item away: it still consumes stock and is recorded as cost (`pos-spec.md` §5).
The event `billing.comp.applied`, the permission `billing.comp.apply` (PIN, manager), the domain
command (`BillCommand::Reduce` with `REDUCTION_KIND_COMP`), the reason action `REASON_ACTION_COMP`
and the projection's fold all exist; nothing produces the event. A **price override** has only its
permission, `billing.price.override` (PIN, manager): no event, no route, no reason. Both change what
a guest pays, so both are money (AGENTS.md §7).

## Options considered

1. **Express both as a discount.** Loses the distinction accounting and fraud review need.
2. **Override by editing the price book for one order.** Configuration is the cloud's; a store never
   owns it.
3. **Two acts on the edge, each with a reason code and an approver** (chosen).

## Decision

1. **Comp, on an open bill.** `POST /api/bills/{id}/comp` names one `order_line_id`, a
   `reason_code_id` published for `REASON_ACTION_COMP` and, when the actor lacks the permission, an
   approver's PIN. The amount is the edge's: the line's whole total, modifiers included. It records
   `billing.comp.applied` (unchanged) and, with an approver, `security.permission.overridden`. A
   line is comped at most once. The receipt lists the line and its comp. Stock is consumed as sold.
2. **Price override, on a line before its bill opens** (lines are frozen once a bill is open,
   ADR-0146). `POST /api/lines/{id}/price` takes the new `unit_price` (an integer in minor units,
   above zero, at most the store's published multiple of the published price, 100 by default), a `reason_code_id` published for a new
   additive reason action `REASON_ACTION_PRICE_OVERRIDE`, and an approver as above. It records a new
   event, `sales.order_line.repriced` (version 1, additive: `order_id`, `order_line_id`,
   `unit_price`, `previous_unit_price`, `line_total`, `reason_code_id`). Tax and promotions are
   computed from the new price as from any price. A zero price is refused: giving away is a comp.
3. **Tax.** A comp lowers the taxable base exactly as a discount does (`billing::assemble` already
   allocates it so), unless a country pack says otherwise; a country that taxes a giveaway at its
   market value states that in its pack, and the accountant confirms each country in phase 7.
4. **Two catalogue acts**, `billing.line.comp` and `sales.line.reprice`, effect `ACTION_EFFECT_MONEY`
   with predictable refusals (permission or approver, a reason, an open bill or no bill yet).

## Configuration

- **A setting** (ADR-0160): `billing.price_override_max_multiple`, a whole number from 1 to 100,
  **default 100**: the most a new unit price may be, as a multiple of the published price.
- **Who may comp or override, and with whose approval,** is each role's permissions
  (`billing.comp.apply`, `billing.price.override`, both PIN-flagged by default, ADR-0158), and why is
  the store's published reason list (`REASON_ACTION_COMP`, `REASON_ACTION_PRICE_OVERRIDE`,
  ADR-0115): configuration already. Every template keeps the defaults.
- **How a comp is taxed** is the country pack's (law), confirmed per country in phase 7.
- **Fixed rules:** a reason and the events on every comp and override, `security.permission.overridden`
  whenever an approver stood in (audit); a whole-line comp, an override only before the bill opens,
  and no zero price (money integrity: a bill covers frozen lines, and a giveaway is a comp).

## Consequences accepted

- One new event, one additive reason action, two routes and two acts; snapshots grow.
- Partial comps (one of three) need the line's quantity changed before the bill opens; a comp of
  part of a line is not offered.
