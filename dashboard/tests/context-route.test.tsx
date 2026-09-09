// What a URL is allowed to say about the working context, and what it is not
// ([ADR-0120](../../docs/adr/0120-navigation-preserves-the-working-context.md)).
//
// The defect these pin was a three-part chain, and each part looked correct alone. `screenHref`
// appends `?store=` only for a screen declaring `scope: "store"` — 7 of 26. `TenantContext` read an
// absent `?store=` as an instruction to clear. `setStoreId` writes through to `localStorage`. So
// walking from a store-scoped screen to any of the other nineteen destroyed the store the operator
// had chosen, in the signal *and* in the remembered copy, and walking back told them to "Choose the
// store in the top bar to continue".
//
// The fix is one distinction: a URL that *names* a store asserts it; a URL that says nothing asserts
// nothing. These drive the real `TenantContext` — a probe re-creating the effect would assert a copy
// of the rule, and a copy is free to drift from it.

import { createMemoryHistory, MemoryRouter, Route } from "@solidjs/router";
import { cleanup, render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { TenantContext } from "../src/App";
import {
  selectStore,
  selectTenant,
  storeId,
  storeName,
  tenantId,
  tenantName,
} from "../src/state/session";

const TENANT_VN = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const TENANT_JP = { id: "01M2222222222222222222222J", name: "Pizza 4P's Japan" };
const STORE_LTT = { id: "01M221BB8BB5SESQDB895SJHJS", name: "4P's Le Thanh Ton" };
const STORE_BT = { id: "01M221KB3CN3XVWHY1NSCKA48H", name: "4P's Ben Thanh" };

/**
 * Mounts the real `TenantContext` under the route shape `App` gives it, at one URL.
 *
 * The history is seeded before `render` rather than passed as `initialEntries` — this router matches
 * against a `history` object, and a `MemoryRouter` without one starts at `/`, where the nested route
 * never mounts and the effect under test never runs. That failure is silent: every assertion about
 * *not* clearing the store passes vacuously, which is the wrong half to have pass by accident.
 * `screen` is a concrete child path for the same reason — a wildcard does not mount the parent here.
 */
function arriveAt(url: string, screen: string) {
  const history = createMemoryHistory();
  history.set({ value: url });
  const mounted = render(() => (
    <MemoryRouter history={history}>
      <Route path="/t/:tenant" component={TenantContext}>
        <Route path={screen} component={() => <span>screen</span>} />
      </Route>
    </MemoryRouter>
  ));
  return mounted;
}

/** Waits until the route has actually mounted, so no assertion below can pass vacuously. */
async function mounted(view: ReturnType<typeof arriveAt>) {
  await waitFor(() => expect(view.container.innerHTML).toContain("screen"));
}

describe("what a URL says about the working context", () => {
  beforeEach(() => {
    localStorage.clear();
    selectTenant("", "");
  });
  afterEach(cleanup);

  it("takes the store from a URL that names one", async () => {
    selectTenant(TENANT_VN.id, TENANT_VN.name);
    await mounted(arriveAt(`/t/${TENANT_VN.id}/devices?store=${STORE_BT.id}`, "/devices"));
    await waitFor(() => expect(storeId()).toBe(STORE_BT.id));
  });

  // The reported bug, as a property: the chosen store survives a screen that does not use one.
  // Before ADR-0120 this ended with an empty `storeId()` and the "choose a store" panel.
  it("leaves the remembered store alone on a URL that names none", async () => {
    selectTenant(TENANT_VN.id, TENANT_VN.name);
    selectStore(STORE_LTT.id, STORE_LTT.name);
    await mounted(arriveAt(`/t/${TENANT_VN.id}/translations`, "/translations"));
    await waitFor(() => expect(tenantId()).toBe(TENANT_VN.id));
    expect(storeId()).toBe(STORE_LTT.id);
    expect(storeName()).toBe(STORE_LTT.name);
  });

  // Decision (3). Silence must not become "carry it anywhere": a store belongs to a tenant, so
  // arriving under a different one drops it. Shipping decision (1) alone would have left tenant VN's
  // shop in scope while the console showed tenant JP.
  it("drops the store when the URL names a different tenant", async () => {
    selectTenant(TENANT_VN.id, TENANT_VN.name);
    selectStore(STORE_LTT.id, STORE_LTT.name);
    await mounted(arriveAt(`/t/${TENANT_JP.id}/translations`, "/translations"));
    await waitFor(() => expect(tenantId()).toBe(TENANT_JP.id));
    expect(storeId()).toBe("");
    expect(storeName()).toBe("");
    // ADR-0065's D2 rule, still in force: an id with no name beside it must not keep the old name.
    expect(tenantName()).toBe("");
  });

  // Both halves in one navigation: a different tenant *and* a store of its own. The clear must not
  // eat the assertion, whichever order the effect runs them in.
  it("takes a store named alongside a different tenant", async () => {
    selectTenant(TENANT_VN.id, TENANT_VN.name);
    selectStore(STORE_LTT.id, STORE_LTT.name);
    await mounted(arriveAt(`/t/${TENANT_JP.id}/devices?store=${STORE_BT.id}`, "/devices"));
    await waitFor(() => expect(tenantId()).toBe(TENANT_JP.id));
    await waitFor(() => expect(storeId()).toBe(STORE_BT.id));
  });

  // An empty `?store=` is a URL *saying* "no store", which is different from saying nothing. Kept
  // separate from the absent case because the two differ by one `undefined` check.
  it("clears the store when the URL names an empty one", async () => {
    selectTenant(TENANT_VN.id, TENANT_VN.name);
    selectStore(STORE_LTT.id, STORE_LTT.name);
    await mounted(arriveAt(`/t/${TENANT_VN.id}/devices?store=`, "/devices"));
    await waitFor(() => expect(storeId()).toBe(""));
  });
});
