# ADR-0221 — A bar tab is an order without a table that stays open, named by a number

**Status** Proposed · **Owner** @maintainers-architecture · **Date** 2026-11-18
· Needed by [ADR-0200](0200-six-business-type-templates-ship-with-the-framework.md) decision 5 ·
Relates to [ADR-0146](0146-a-counter-store-starts-its-own-orders.md),
[ADR-0194](0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
[ADR-0206](0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md),
[ADR-0220](0220-a-bill-takes-its-payments-one-at-a-time.md)

Waits on the owner: whether a tab may carry a guest's name, and whether holding a card on a tab
waits for the card terminal (phase 8).

## Context

A bar opens a tab for a guest, adds rounds through the night, and settles once. The capability
`tabs_enabled` exists and nothing reads it. A tableless order already exists (ADR-0146), fires
and settles like a table's. What a tab adds is a way to find it again, on any till, among many,
and that way is usually the guest's name, which is personal data (Decree 13/2023, GDPR) and may
not ride an event or a log.

## Options considered

1. **A tab is a table** created on the fly. Pollutes the floor plan and its reports.
2. **A free-text label in the order event.** A name in an event is forbidden (AGENTS.md §2).
3. **A tableless order with a daily tab number; an optional label kept apart** (chosen).

## Decision

1. **Opening a tab** is `POST /api/orders` with `tab: true` on a store with `tabs_enabled`
   (refused otherwise). The edge gives it the next **tab number** of the business day, as it gives a
   queue number, and records it on `sales.order.opened` as an additive optional `tab_number`.
2. **An optional label** (at most 32 characters, typed by staff) is personal data: the edge keeps it
   in its subject store (ADR-0107) under the order, never in an event, a log or the cloud, and
   masks it when the business day ends plus the store's `default_retention_days` (ADR-0206's own
   clock). The till shows it beside the number; it is never printed on a kitchen ticket.
3. **Every till lists the open tabs** (`GET /api/tabs`: number, label, total, last round's time),
   and a tab takes rounds and fires exactly as an order does. A tab may be moved to a table (the
   existing transfer); a table's order does not become a tab.
4. **Closing a tab** is the bill and payment of any order, including payments taken one at a time
   (ADR-0220).
5. **No card is held.** Pre-authorising a card needs the card terminal (ADR-0154, phase 8).
6. **Shift and day.** Closing a shift lists the open tabs as a warning; a tab open when the business
   day ends stays open and is listed on the next day's first shift.
7. **Catalogue:** `sales.tab.open` (`ACTION_EFFECT_EDGE_WRITE`) and a `tabs_list` component.

## Consequences accepted

- One additive field on `sales.order.opened`; one route plus a field on an existing one; a label in
  the edge's subject store with its own expiry and erasure path. The DPIA register gains the label.
- A guest who wants no name on the tab is given a number only; staff are told so on the screen.
