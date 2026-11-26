# ADR-0255 — A QR order's automatic send keeps the hours the store sets

**Status** Accepted 2026-11-26 under the owner's decisions · **Owner** @maintainers-architecture ·
**Date** 2026-11-26 · Phase 11 (open item O4, time windows) · Adds settings to the `qr` node
(`AGENTS.md` §7) · Completes decision 5 of
[ADR-0247](0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md) · Relates to
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)

## Context

ADR-0247 lets a store send a guest's QR order to the kitchen with no tap (`AUTO_ACCEPT_AND_FIRE`), and
left the owner's "only in some hours" as a follow-up: a store may want no-tap sending at lunch, when
staff are busy, and a person to look at each order late at night. ADR-0247 does not say what the hours
look like, where they live, or what happens outside them.

## Options considered

1. **A list of windows per day of the week.** Covers every case, but needs a new setting kind, its
   console editor and its own validation, for a need nobody has stated.
2. **A new `kitchen` node shared by QR and vendor orders.** A new signed node and its plumbing, for
   one window.
3. **One daily window on the `qr` node, as two whole-minute settings, and a setting for the mode
   outside it.** Uses the register's existing `INT` and `CHOICE` kinds, and reads like the node's
   own `business_hours`.

## Decision

1. **Option 3.** Three settings on the `qr` node, scoped as `qr.kitchen_release` is (tenant, brand,
   store group, store):
   - `qr.auto_fire_from_minute` and `qr.auto_fire_until_minute`: whole minutes after the store's
     local midnight, 0 to 1439, unit `MINUTES`. The window is `[from, until)`. A `from` later than
     `until` wraps past midnight, and **equal values mean always**, as `business_hours` reads equal
     hours. Both default to 0, so the default is "always": today's behaviour.
   - `qr.kitchen_release_outside_hours`: the mode an order takes outside the window,
     `KITCHEN_RELEASE_CONFIRM_FIRES` or `KITCHEN_RELEASE_CONFIRM_THEN_SEND`. The default is
     `CONFIRM_THEN_SEND`, the safe choice: a person sees the order first.
2. **The window narrows only `AUTO_ACCEPT_AND_FIRE`.** Under the other two modes the settings change
   nothing.
3. **The edge decides, when the order arrives.** It reads its `ClockSource` and the store's own
   time zone from its published configuration, never the device's clock or a fixed offset. An order
   that arrives inside the window is accepted and fired as ADR-0247 says. One that arrives outside
   it waits as the outside mode says, and keeps that mode when the window opens later.
4. **Vendor connections are not changed.** Each keeps its own `kitchen_release`; a window for them
   is added when a store asks for one, on the connection, in the same shape.

## Consequences accepted

- Three more settings in the register and the console. The defaults change nothing.
- One window a day. A store that needs different hours on different days needs a later ADR
  (option 1).
- The hours are read when the order arrives. An order that waited outside the window is not sent
  when the window opens; staff send it as the outside mode says.
