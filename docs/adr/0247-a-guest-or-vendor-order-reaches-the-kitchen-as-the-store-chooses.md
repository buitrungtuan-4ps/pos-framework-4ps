# ADR-0247 — How a guest or vendor order reaches the kitchen is chosen per channel, vendor and store

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (open item O4) · **Amends decision 3 of
[ADR-0116](0116-the-qr-hold-is-derived-and-it-gates-firing.md)** · Relates to
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md),
[ADR-0215](0215-a-delivery-order-names-its-vendor.md)

## Context

A QR or delivery order waits for staff; the till's button "Send to kitchen" only confirms it, and a
second send is needed before the kitchen sees it (phase 10, F5). The owner wants the store to choose.

## Decision

1. **A choice with three modes**, `KITCHEN_RELEASE_*`:
   - `AUTO_ACCEPT_AND_FIRE`: no tap. The order is accepted on arrival and every line is fired in the
     same transaction as the order.
   - `CONFIRM_FIRES`: one tap. Confirming accepts the order and fires every unsent line in the same
     transaction. The button reads "Accept and send to kitchen".
   - `CONFIRM_THEN_SEND`: two steps, today's behaviour and **the default**. The button reads "Accept
     order"; the lines are then sent as any order's are.
2. **Where it is set** (ADR-0160 scopes: tenant, brand, store group, store):
   - `qr.kitchen_release` for QR orders;
   - `kitchen_release` on each vendor connection, for its delivery orders, defaulting to
     `CONFIRM_THEN_SEND`.
   Kiosk orders are paid before they are sent and are not held; a kiosk setting waits until a kiosk
   order needs one.
3. **The hold follows.** Under `AUTO_ACCEPT_AND_FIRE` the order is not held for staff (ADR-0116
   decision 1 gains this fourth input). `qr.staff_confirmation_required` off still means no hold and a
   manual send, as before.
4. **Firing is the kitchen's ordinary fire**: the same routing, station plan, flow guards and events.
   An automatic fire is recorded under the device that accepted the order, never a person.
5. **Time windows** (auto-fire only in some hours) are left as a follow-up.

## Consequences accepted

- Two settings and a changed button label. The default changes nothing.
- Under `AUTO_ACCEPT_AND_FIRE` a passer-by's QR order reaches the kitchen unseen; the guardrails of
  ADR-0057 (table token, rate limit, hours) still apply. The console says so beside the setting.
