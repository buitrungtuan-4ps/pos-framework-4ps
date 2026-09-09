// The working context: what the top bar names, and what every screen scopes its calls to.
//
// These pin Wave 3 · D2. There are two ways a tenant or store enters the context and they are not
// symmetrical: the picker hands over an id *and* a name (`selectTenant`/`selectStore`), while the
// URL hands over an id alone (`setTenantId`/`setStoreId`, called by `TenantContext` on every
// navigation). Before the fix the second path wrote the id and left the remembered name untouched,
// so following a link for one shop while another was remembered put the wrong shop's name above the
// right shop's data — over a configuration publish, a till retirement, a revenue read.

import { beforeEach, describe, expect, it, vi } from "vitest";

const STORE_A = { id: "01M221BB8BB5SESQDB895SJHJS", name: "4P's Le Thanh Ton" };
const STORE_B = { id: "01M221KB3CN3XVWHY1NSCKA48H", name: "4P's Ben Thanh" };
const TENANT_VN = { id: "01M22190WCY5PS7KCA7ET7H679", name: "Pizza 4P's Vietnam" };
const TENANT_JP = { id: "01M2222222222222222222222J", name: "Pizza 4P's Japan" };

// The module holds its signals at module scope and mirrors them into `localStorage`, so each test
// re-evaluates it against a clean store rather than trying to reset shared signals. A cache-busting
// query string on the import specifier would be simpler but breaks the TypeScript transform — Vite
// infers the loader from the extension, and `session.ts?fresh=1` is no longer `.ts`.
async function freshSession() {
  localStorage.clear();
  vi.resetModules();
  return await import("../src/state/session");
}

/** The same module without clearing storage: what a page reload looks like from here. */
async function reloadSession() {
  vi.resetModules();
  return await import("../src/state/session");
}

describe("the working context", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("names the store the operator picked", async () => {
    const session = await freshSession();
    session.selectStore(STORE_A.id, STORE_A.name);
    expect(session.storeId()).toBe(STORE_A.id);
    expect(session.storeName()).toBe(STORE_A.name);
  });

  // The defect, in one test: pick a store, then follow a link naming a different one.
  it("does not keep the previous store's name when a link names another store", async () => {
    const session = await freshSession();
    session.selectStore(STORE_A.id, STORE_A.name);

    session.setStoreId(STORE_B.id);

    expect(session.storeId()).toBe(STORE_B.id);
    expect(session.storeName()).not.toBe(STORE_A.name);
  });

  it("keeps the name when the link names the store already in context", async () => {
    const session = await freshSession();
    session.selectStore(STORE_A.id, STORE_A.name);

    // `TenantContext` re-affirms the context from the URL on every navigation, so this is the
    // ordinary case and it must not blank a name the operator can see.
    session.setStoreId(STORE_A.id);

    expect(session.storeName()).toBe(STORE_A.name);
  });

  it("does not keep the previous tenant's name when a link names another tenant", async () => {
    const session = await freshSession();
    session.selectTenant(TENANT_VN.id, TENANT_VN.name);

    session.setTenantId(TENANT_JP.id);

    expect(session.tenantId()).toBe(TENANT_JP.id);
    expect(session.tenantName()).not.toBe(TENANT_VN.name);
  });

  it("lets the picker fill in a name the URL could not carry", async () => {
    const session = await freshSession();
    session.setStoreId(STORE_B.id);
    expect(session.storeName()).toBe("");

    session.setStoreName(STORE_B.name);

    expect(session.storeId()).toBe(STORE_B.id);
    expect(session.storeName()).toBe(STORE_B.name);
  });

  // ADR-0120 §3. The two ways a tenant enters the context used to disagree: `selectTenant` cleared
  // the store, because a store belongs to a tenant, and `setTenantId` — the URL's way in — cleared
  // only the name. The disagreement was masked while `TenantContext` cleared the store on every
  // navigation, and making an absent `?store=` mean silence is what exposes it, so the invariant
  // moved here where both paths honour it.
  it("drops the store when the URL changes tenant", async () => {
    const session = await freshSession();
    session.selectTenant(TENANT_VN.id, TENANT_VN.name);
    session.selectStore(STORE_A.id, STORE_A.name);

    session.setTenantId(TENANT_JP.id);

    expect(session.storeId()).toBe("");
    expect(session.storeName()).toBe("");
  });

  it("keeps the store when the URL re-affirms the same tenant", async () => {
    const session = await freshSession();
    session.selectTenant(TENANT_VN.id, TENANT_VN.name);
    session.selectStore(STORE_A.id, STORE_A.name);

    // What every navigation within one tenant does — `TenantContext` re-affirms the path's tenant.
    session.setTenantId(TENANT_VN.id);

    expect(session.storeId()).toBe(STORE_A.id);
    expect(session.storeName()).toBe(STORE_A.name);
  });

  // The clear has to reach the remembered copy too, or the next reload resurrects a shop that
  // belongs to another tenant.
  it("forgets the dropped store across a reload", async () => {
    const first = await freshSession();
    first.selectTenant(TENANT_VN.id, TENANT_VN.name);
    first.selectStore(STORE_A.id, STORE_A.name);
    first.setTenantId(TENANT_JP.id);

    const reloaded = await reloadSession();

    expect(reloaded.tenantId()).toBe(TENANT_JP.id);
    expect(reloaded.storeId()).toBe("");
  });

  it("remembers the context across a reload", async () => {
    const first = await freshSession();
    first.selectTenant(TENANT_VN.id, TENANT_VN.name);
    first.selectStore(STORE_A.id, STORE_A.name);

    const reloaded = await reloadSession();

    expect(reloaded.tenantId()).toBe(TENANT_VN.id);
    expect(reloaded.storeName()).toBe(STORE_A.name);
  });
});
