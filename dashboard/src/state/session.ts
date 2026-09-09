// The dashboard's small amount of client state: whether a super-admin session is live, and the
// tenant/store the operator is currently working within. Most `/admin` endpoints are tenant-scoped
// (ADR-0037) and many name a store, so the operator sets that context once and every screen reads
// it. The context is a per-browser convenience, remembered in localStorage; it is neither a secret
// nor authoritative — the server's session cookie is the only thing that authorises anything.

import { createSignal } from "solid-js";

import type { AdminIdentity } from "../api/types";

const TENANT_KEY = "pos.dashboard.tenant";
const STORE_KEY = "pos.dashboard.store";
const TENANT_NAME_KEY = "pos.dashboard.tenantName";
const STORE_NAME_KEY = "pos.dashboard.storeName";

function load(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    // Private windows and blocked site-data throw on access; an empty context is the safe default.
    return "";
  }
}

function save(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Persistence is a convenience; a failure to store is not an error worth surfacing.
  }
}

const [authed, setAuthed] = createSignal(false);
export { authed, setAuthed };

// The signed-in admin's own identity (id/email/name/role/status), fetched from `/admin/whoami` once
// the Shell mounts (ADR-0067, Track G1). It is a display and nav-gating convenience — `null` until it
// loads, and cleared on sign-out — never an authorisation; the server re-checks every route's role.
const [actingAdmin, setActingAdmin] = createSignal<AdminIdentity | null>(null);
export { actingAdmin, setActingAdmin };

// The names behind the ids in context, so the top bar shows "Bến Thành" rather than a raw ULID
// (ADR-0065). They are display convenience remembered per browser; the id is what every screen reads
// and what actually scopes a call.
const [tenantName, setTenantNameSignal] = createSignal(load(TENANT_NAME_KEY));
export { tenantName };

const [storeName, setStoreNameSignal] = createSignal(load(STORE_NAME_KEY));
export { storeName };

const [tenantId, setTenantIdSignal] = createSignal(load(TENANT_KEY));
export { tenantId };

export function setTenantId(next: string): void {
  const trimmed = next.trim();
  // A *different* tenant invalidates the remembered name. This is the URL's way in — `TenantContext`
  // calls it on every navigation — and unlike `selectTenant` it is handed an id with no name beside
  // it. Before this cleared, opening a link for one tenant while another was remembered left the top
  // bar reading the old tenant's name over the new tenant's data (Wave 3 · D2). An empty name reads
  // as the picker's placeholder, which is honest; a stale one is a lie, and the console publishes
  // configuration under it.
  if (trimmed !== tenantId()) {
    setTenantNameSignal("");
    save(TENANT_NAME_KEY, "");
  }
  setTenantIdSignal(trimmed);
  save(TENANT_KEY, trimmed);
}

const [storeId, setStoreIdSignal] = createSignal(load(STORE_KEY));
export { storeId };

export function setStoreId(next: string): void {
  const trimmed = next.trim();
  // Same reason as `setTenantId`, and this is the one that bites hardest: the store scopes the
  // configuration publish, the till retirement, the tax authoring and the revenue read, so a header
  // naming the wrong shop over any of those is worse than a header naming none (Wave 3 · D2).
  if (trimmed !== storeId()) {
    setStoreNameSignal("");
    save(STORE_NAME_KEY, "");
  }
  setStoreIdSignal(trimmed);
  save(STORE_KEY, trimmed);
}

/**
 * Fills in the display name for a context that arrived as a bare id — a shared link, a bookmark, a
 * second tab. The picker resolves it from the registry it already reads (Wave 3 · D2); nothing here
 * touches the id, because the id is what scopes every call and the URL is its only authority.
 */
export function setTenantName(name: string): void {
  setTenantNameSignal(name);
  save(TENANT_NAME_KEY, name);
}

/** The store's half of [`setTenantName`]. */
export function setStoreName(name: string): void {
  setStoreNameSignal(name);
  save(STORE_NAME_KEY, name);
}

/**
 * Selects the working tenant by id and name (from the registry picker), and clears any store in
 * context — a store belongs to a tenant, so a store chosen under the old tenant no longer applies.
 */
export function selectTenant(id: string, name: string): void {
  const trimmed = id.trim();
  setTenantIdSignal(trimmed);
  setTenantNameSignal(name);
  save(TENANT_KEY, trimmed);
  save(TENANT_NAME_KEY, name);
  setStoreIdSignal("");
  setStoreNameSignal("");
  save(STORE_KEY, "");
  save(STORE_NAME_KEY, "");
}

/** Selects the working store by id and name (from the registry picker), within the current tenant. */
export function selectStore(id: string, name: string): void {
  const trimmed = id.trim();
  setStoreIdSignal(trimmed);
  setStoreNameSignal(name);
  save(STORE_KEY, trimmed);
  save(STORE_NAME_KEY, name);
}
