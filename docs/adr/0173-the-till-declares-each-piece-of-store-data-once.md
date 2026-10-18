# ADR-0173 — The till declares each piece of store data once

**Status** Accepted · **Owner** @maintainers-architecture · **Date** 2026-10-14
· Relates to [ADR-0109](0109-counting-the-taps-an-operator-makes.md),
[ADR-0115](0115-reason-codes-are-a-managed-list.md),
[ADR-0158](0158-the-till-enforces-each-persons-own-permissions.md),
[ADR-0160](0160-everything-a-store-runs-differently-is-published-configuration.md)

owner delegated approval 2026-10-14

## Context

The till (`ui/`) reads what it draws from the edge through eleven loaders. Each one decides by
itself when it runs, what a failure does and what happens on a reconnect:

| Data | Loader | Read | Called from |
|---|---|---|---|
| Floor, areas, stations, table states | `loadFloor` | `GET /api/floor` | `loadConfiguration` |
| The store's price book | `loadMenu` | `GET /api/menu` | `loadConfiguration` |
| The walk-in price books | `loadMenu` | `GET /api/menu?channel=…`, takeaway always, dine-in where walk-ins are eaten in | `loadConfiguration` |
| Button plan | `loadLayout` | `GET /api/layout` | `loadConfiguration` |
| Money and language settings | `loadLocale` | `GET /api/locale` | `loadConfiguration` |
| Reason codes | `loadReasonCodes` | `GET /api/reason-codes` | `loadConfiguration` |
| Session, idle lock, permissions | `loadSession` (`state/session.ts`) | `GET /api/session` | `loadConfiguration`, the idle lock, and the boot gate in `App.tsx`, which reads it once more on its own |
| Drawers | `loadDrawers` | `GET /api/shifts` | `loadConfiguration` |
| Live orders, kitchen orders | `loadLiveOrders` | `GET /api/orders/live`, `GET /api/orders/kitchen` | `loadStore`, a `resync` |
| The open shift | `loadShift` | `GET /api/shifts/current` | `loadStore` |
| Queue numbers | `refreshQueueNumbers` | `GET /api/orders/open` | the kitchen board |

`loadStore` runs them at the boot gate and at sign-in. `loadConfiguration` runs again on every
`config_applied` and every `resync`, and re-reads all seven of its loaders whatever changed. The
status bar polls `GET /api/sync` on its own timer. Screens make their own reads too: Today, Devices,
Confirm, Takeaway, Setup and Claim.

Every loader is "forgiving", so a failed read keeps what was held and nobody is told. On a first
boot nothing is held, so a failed read leaves the compiled-in fallback floor or an empty price book.
The operator sees a till that looks ready and is not. Nothing retries until the next boot, sign-in
or configuration change. A reload starts empty and waits for every read before it draws anything.
Every act waits for the edge's answer before the screen moves, including adding a line, the act an
operator makes most often. (`Order.tsx` describes that act as optimistic; it is not.)

The edge already holds its own hot paths to budgets each night (`tests/perf_budget.rs`). Nothing
measures what an operator feels: the time from a tap to the screen that answers it.

## Options considered

| | Option | Cost |
|---|---|---|
| A | Keep the loaders and fix each one's gaps | Eleven places to add retry, a banner and reconnect handling, and they keep drifting apart |
| B | A data-fetching library (TanStack Query for Solid) | A new runtime dependency and its release cadence, for a cache shape the till only half uses. It has no idea of a boot gate, of `config_applied` or of the edge's refusals |
| C | **One registry and one small engine in `ui/`** | About 400 lines the till owns, and a migration of every loader |

## Decision

Option **C**.

1. **One registry.** `ui/src/state/registry.ts` declares each piece of store data once:
   - its key and its loader, the existing `api.*` read;
   - how a body is applied to the store;
   - whether it is **required** to draw the till or **optional**;
   - whether it is **persisted**, and through which projection;
   - which configuration nodes it derives from;
   - which events refetch it.

   The entries are the floor, the store's price book, the walk-in price books, layout, locale,
   reason codes, session and permissions, drawers, the open shift and live orders.
   - **Required:** session and permissions, the store's price book, locale, floor and live orders.
   - **Optional:** everything else. An optional entry that never loads falls back as it does today.
     An empty node is a real answer ([ADR-0115](0115-reason-codes-are-a-managed-list.md)); a failed
     read is not, and is shown as failed.

   The status poll and the screen-owned reads stay outside the registry, but use the engine's
   retry and banner.
2. **One engine.** `ui/src/state/engine.ts` runs every entry the same way:
   - **States.** Each entry is `loading`, `loaded`, `stale` (drawn from the snapshot, revalidating)
     or `error`, with the refusal's reason.
   - **Retry with backoff.** After 0.5 s, then doubling to a 15 s ceiling, with ±20% jitter.
     - Only a transport failure, a `5xx`, `408` or `429` is retried.
     - A `401` sends the device to pair and a `403` to sign in, as `request` does today.
     - A `404` from an edge older than the route marks the entry *not offered*, not failed.
   - **Refetch triggers.**
     - When the live link reconnects: the live entries, and every configuration entry revalidated.
     - On a `resync`: the live entries.
     - On `config_applied`: the entries whose nodes the frame names.
   - **Scoped `config_applied`.** The edge adds an optional `changed_nodes` to the frame: the
     top-level nodes whose content differs from the document it ran before.
     - An absent field, or a node no entry claims, refetches every configuration entry.
     - Over-fetching is therefore always safe, and with item 3 an unchanged entry costs one `304`.
     - The frame is the edge's own (`fanout.rs`), not `pos-proto`, and the field is additive.
   - **ETag and `304`.** The edge answers each configuration read with a strong `ETag`, a SHA-256
     of the body. It answers `If-None-Match` with `304` and no body, through the helper the asset
     routes already use. No new dependency, and the route shapes are unchanged.
   - **Boot gate.** The till draws once every required entry is `loaded` or `stale`. Until then it
     shows what it is waiting for. A required entry in `error` shows a banner naming it, with a
     **Retry** button that resets the backoff.
   - **Disabled buttons say why.** A button whose act needs an entry that is not loaded is
     disabled, and says why in visible text tied to it by `aria-describedby`. For example,
     "Reason codes are still loading" or "Reason codes could not be loaded".
   - **Country-neutral.** The engine is the same in every country:
     - It knows no currency, locale, tax or country. Locale is one entry among the others, and money
       is still formatted from the published `locale` node.
     - Every sentence it shows is a translation key.
     - Its timings are constants about networks, not about any market.
3. **A configuration snapshot.** For stale-while-revalidate, the till keeps an IndexedDB snapshot
   of the persisted entries.
   - **What it holds:** the floor's layout (areas, tables, stations), both kinds of price book,
     layout, locale and reason codes. Each is stored with its `ETag`, the time it was saved and a
     schema version.
   - **Configuration only.** The floor's projection drops table states and seated times. Session,
     permissions, PINs, device and session tokens, staff and customer data, drawers (which name an
     assigned employee), shifts and live orders are declared not persistable. The engine refuses to
     write an entry not marked persisted, and a test pins the persisted set.
   - **Retention:**
     - Each entry is replaced by the next successful read.
     - Entries older than 7 days, or of another schema version, are ignored and deleted.
     - The whole snapshot is deleted when a person signs out, and when the device is unpaired,
       whether by a reset or by a `401` that clears its token.
     - An idle lock is not a sign-out for this purpose. It keeps the device in the same service,
       and it is when a fast redraw matters most.
   - **No new dependency.** IndexedDB is used through the browser API directly.
4. **Optimistic writes, for an allow-list of three acts.**
   - **The acts:** adding a line (with its modifiers and note), changing a quantity, and moving a
     table.
   - **How it reconciles.** The screen paints the change at once, marked as sending, and sends one
     request.
     - The edge's answer, or its fan-out event (whichever lands first), replaces the provisional
       line, matched by the request it came from.
     - A refusal rolls the change back and shows the refusal's own sentence.
     - A failure with no answer rolls it back and re-reads the order, because the edge may have
       committed it.
   - **Totals stay the edge's.** A provisional line shows its unit price. The bill's totals and tax
     stay the last ones the edge confirmed, and payment is disabled, with its reason, while any
     write is pending.
   - **Everything else waits for the edge.** That includes payment and settle, voids, discounts,
     fee waivers, refunds, comps, every drawer and cash act, firing to the kitchen, and splitting
     or merging bills. Adding an act to the list needs a new ADR.
5. **No device-side write queue.** The edge is the only writer. An act the edge did not answer
   fails now and says so, and the till never re-sends it later. A store whose cloud link is down
   keeps selling through the edge's outbox
   ([ADR-0137](0137-a-deep-outbox-warns-and-never-refuses.md)). That is the queue, and there is
   one.
6. **A tap-to-paint budget, checked nightly.** A new job in `nightly.yml` builds the edge in release
   mode and the till for production. It drives headless Chromium through the main acts, and fails
   when a 95th percentile exceeds its budget.
   - **What it measures:** from the tap's `pointerdown` timestamp to the first animation frame after
     the answering change is in the DOM.
   - **Budgets:**
     - **50 ms** for an optimistic act and for switching screens over loaded data;
     - **150 ms** for an act that waits for the edge;
     - **300 ms** from a reload to a till drawn from the snapshot.
   - **When the budgets apply.** The measurement pull request records today's figures. A budget is
     enforced once the change it measures has landed.
   - **Per-commit checks are unchanged.** ADR-0109 holds: the per-commit replay still asserts no
     timing, for the reason it gives.

## Consequences accepted

- Every loader moves into the registry. `loadStore` and `loadConfiguration` become calls into the
  engine, which changes the shape of `store.ts` and `session.ts`.
- **On the edge, two additive changes:** an `ETag` header with `304` on five configuration reads
  (floor, menu, layout, locale and reason codes), and an optional `changed_nodes` on
  `config_applied`. An older edge sends neither, and the till falls back to full reads.
- **Personal data.** The snapshot is configuration only. It holds no personal data under Vietnam's
  Decree 13/2023 (PDPD), the GDPR or the CCPA, and it never leaves the device. So it needs no
  lawful basis, consent or DPIA of its own, and it involves no cross-border transfer. It does hold
  the store's published prices, which are confidential business data; clearing the snapshot on
  sign-out and on unpairing limits how long a device keeps them. Any later entry that would carry
  personal data cannot be persisted without a new ADR.
- An operator can see a provisional line that the edge then refuses. The rollback and the
  refusal's sentence are the cost of not waiting for the round trip.
- A nightly browser job adds a few minutes to the night and one more set of numbers to keep honest.
