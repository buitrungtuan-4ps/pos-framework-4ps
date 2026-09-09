# ADR-0120 — An absent `?store=` is silence, not a denial

**Status** Accepted · **Owner** @maintainers-cloud · **Last reviewed** 2026-09-09
**Relates to** [ADR-0037](0037-api-keys.md) (tenant-bound, deny-by-default: why a scoped read needs the tenant in the first place) · [ADR-0065](0065-cloud-org-registry.md) (the picker resolves an id to a name, so the top bar can say "Bến Thành") · [ADR-0099](0099-store-hub.md) (the front page a chosen context lands on) · [`docs/cloud-admin-ux-plan.md`](../cloud-admin-ux-plan.md) §3w.1 D2 (the sibling defect: the top bar naming the wrong store) and its "Correction — the URL shape" note (which introduced the shape this amends)

**Context.** The console's working context is a tenant and, for seven of twenty-six screens, a store. An operator sets it once in the top bar and every scoped screen reads it. Two things carry it: the URL (`/t/<tenant>?store=<store>`), so a console link is shareable and two tabs can sit on different tenants; and `localStorage`, described in `state/session.ts` as the *memory* of the last context.

The memory does not work. Navigating through the console destroys it, and the destruction is in the code on purpose.

`state/screens.ts:365` appends the store to a link only for a screen that declares `scope: "store"`:

```js
if (store && spec.scope === "store") return `${base}?store=…`;
```

The distribution is **7 store-scoped screens against 19 tenant-scoped ones**. So a link to any of those nineteen carries no `?store=`.

`App.tsx:220` then reads an absent `?store=` as an instruction to clear:

```js
const store = search.store ?? "";
if (store !== storeId()) setStoreId(store);   // "" → and setStoreId writes it through to localStorage
```

The two together are the defect. Choose a store on Devices; open Translations — a tenant-scoped screen, so no `?store=` — and the store is gone from the signal *and* from `localStorage`. Return to Devices and the screen says **"Choose the store in the top bar to continue."** The operator chose it two clicks ago. Nothing warned, because from the code's point of view nothing went wrong.

The comment defending the clear reads: *"the URL says what the context is, and a link without a store means 'no store', not 'whatever was there before'."* That is a correct reading of a **shared link** and a wrong reading of **in-app navigation**, and the code cannot tell the two apart — a `createEffect` on the route sees only the resulting URL.

There is a second, quieter bug behind it. `setTenantId` clears the remembered *name* when the tenant changes (ADR-0065's D2 fix) but leaves the store id alone, while `selectTenant` — the picker's path — clears the store because a store belongs to a tenant. So the two ways a tenant enters the context disagree, and following a link to a different tenant leaves the previous tenant's store in scope. Today the store-clearing in `TenantContext` masks it; removing that clear would expose it.

**Decision.**

1. **An absent `?store=` says nothing.** `TenantContext` sets the store only when the URL *asserts* one — when `search.store` is present. Absence is silence, and silence lets the remembered store stand. This is what makes `localStorage` the memory the module already claims to be.

2. **A present `?store=` still wins, unchanged.** A link naming a store sets that store, and `setStoreId` still drops the remembered name when the id differs, so ADR-0065's D2 fix is untouched: the top bar never names one shop over another shop's data.

3. **A tenant change invalidates the store, and the invariant moves into `setTenantId`.** "A store belongs to a tenant" is a domain rule, not a routing detail, so it belongs beside `selectTenant`'s identical clearing rather than in the router. `setTenantId` now clears the store id and both names when the tenant differs. This is the bug from the Context section, fixed in the same change because decision (1) is what exposes it — shipping (1) alone would carry tenant A's store into tenant B.

4. **`screenHref` is left alone: tenant-scoped links still carry no `?store=`.** The alternative — append the store to all twenty-six — would also fix the reported symptom, and it was rejected. It puts a parameter in a shared link that the recipient's screen does not read, and it makes every such link overwrite the recipient's own store with the sender's, which is the D2 class of problem in a new place. With (1) in force the parameter is not needed for navigation, because the memory survives; it is needed only where it means something, which is exactly where it already is.

**Consequences.**

*What an operator gets.* The context is chosen once and holds across every screen until they change it. The "Choose the store in the top bar" panel now appears only when there genuinely is no store — first run, or after a deliberate switch — which is the only time it was ever meant to appear.

*What a link does.* `/t/T/devices?store=S` puts tenant T and store S in the recipient's context, as before. `/t/T/translations` puts tenant T in context and **leaves the recipient's store as it was**, where it previously cleared it. For a screen that does not read the store this is invisible; the visible part is that following such a link no longer costs the recipient their context. A sender who wants to hand over "this tenant, no store" has no URL for it, which is a real loss of expressiveness — and the store picker is one click, so it is not worth a parameter that would otherwise never be read.

*What two tabs do.* Unchanged for the tenant, which the path still owns. For the store, two tabs on the same tenant now share the remembered store where previously a tenant-scoped tab would clear it for both — `localStorage` is per-origin, so this was already true of every other remembered value, and the tab that cares carries `?store=` in its own URL.

*What this does not do.* It does not make the context authoritative for anything. The server's session cookie remains the only thing that authorises a read, and `/admin` re-checks the tenant on every route (ADR-0037); a context is a convenience that decides which id a screen sends.

**Tested by** `dashboard/tests/session.test.ts` (the tenant change clears the store, and the picker and URL paths agree on it) and `dashboard/tests/context-route.test.tsx` (the three URL shapes: asserts a store, says nothing, changes tenant).
