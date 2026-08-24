// The dashboard's small amount of client state: whether a super-admin session is live, and the
// tenant/store the operator is currently working within. Most `/admin` endpoints are tenant-scoped
// (ADR-0037) and many name a store, so the operator sets that context once and every screen reads
// it. The context is a per-browser convenience, remembered in localStorage; it is neither a secret
// nor authoritative — the server's session cookie is the only thing that authorises anything.

import { createSignal } from "solid-js";

const TENANT_KEY = "pos.dashboard.tenant";
const STORE_KEY = "pos.dashboard.store";

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
