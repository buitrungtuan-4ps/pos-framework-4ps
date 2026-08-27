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

const [tenantId, setTenantIdSignal] = createSignal(load(TENANT_KEY));
export { tenantId };

export function setTenantId(next: string): void {
  const trimmed = next.trim();
  setTenantIdSignal(trimmed);
  save(TENANT_KEY, trimmed);
}

const [storeId, setStoreIdSignal] = createSignal(load(STORE_KEY));
export { storeId };

export function setStoreId(next: string): void {
  const trimmed = next.trim();
  setStoreIdSignal(trimmed);
  save(STORE_KEY, trimmed);
}

// The names behind the ids in context, so the top bar shows "Bến Thành" rather than a raw ULID
// (ADR-0065). They are display convenience remembered per browser; the id is what every screen reads
// and what actually scopes a call.
const [tenantName, setTenantNameSignal] = createSignal(load(TENANT_NAME_KEY));
export { tenantName };

const [storeName, setStoreNameSignal] = createSignal(load(STORE_NAME_KEY));
export { storeName };

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
