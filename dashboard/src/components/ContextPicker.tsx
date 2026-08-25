// The context picker (ADR-0065): the top-bar control that replaces the old free-text Tenant ID /
// Store ID inputs. It reads the org registry and lets the operator pick a tenant, then one of its
// stores, by **name** — so a normal user never sees or types a ULID, and `tenant_id is not a ULID`
// is no longer reachable. The chosen ids are what every screen reads (state/session); the names are
// shown, and the ULID sits underneath, muted, for reference only.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Store, Tenant } from "../api/types";
import { t } from "../i18n";
import { selectStore, selectTenant, storeId, storeName, tenantId, tenantName } from "../state/session";
import { Button } from "./ui";

export function ContextPicker() {
  const [open, setOpen] = createSignal(false);
  const [tenants, setTenants] = createSignal<Tenant[] | null>(null);
  const [stores, setStores] = createSignal<Store[] | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [failed, setFailed] = createSignal(false);

  const loadTenants = async () => {
    setFailed(false);
    setBusy(true);
    try {
      setTenants(await api.listTenants());
    } catch (caught) {
      setFailed(caught instanceof ApiError || caught instanceof Error);
    } finally {
      setBusy(false);
    }
  };

  const loadStores = async (forTenant: string) => {
    setFailed(false);
    setBusy(true);
    setStores(null);
    try {
      setStores(await api.listStores(forTenant));
    } catch (caught) {
      setFailed(caught instanceof ApiError || caught instanceof Error);
    } finally {
      setBusy(false);
    }
  };

  const toggle = () => {
    const next = !open();
    setOpen(next);
    if (next) {
      void loadTenants();
      if (tenantId()) {
        void loadStores(tenantId());
      }
    }
  };

  const chooseTenant = (tenant: Tenant) => {
    selectTenant(tenant.tenant_id, tenant.name);
    void loadStores(tenant.tenant_id);
  };

  const chooseStore = (store: Store) => {
    selectStore(store.store_id, store.name);
    setOpen(false);
  };

  return (
    <div class="relative">
      <button
        type="button"
        aria-label={t("context.change")}
        aria-expanded={open()}
        onClick={toggle}
        class="flex min-h-touch items-center gap-2 rounded-token border border-line bg-surface-raised px-3 text-sm text-ink"
      >
        <span class="text-ink-muted">{t("context.workingIn")}</span>
        <span class="font-medium">{tenantName() || t("context.chooseTenant")}</span>
        <span class="text-ink-muted" aria-hidden="true">·</span>
        <span class="font-medium">{storeName() || t("context.chooseStore")}</span>
        <span class="text-accent" aria-hidden="true">▾</span>
      </button>

      <Show when={open()}>
        <div class="absolute left-0 z-20 mt-1 w-80 rounded-token border border-line bg-surface shadow-lg">
          <div class="flex items-center justify-between border-b border-line px-3 py-2">
            <span class="text-sm font-semibold text-ink">{t("context.workingIn")}</span>
            <Button variant="secondary" onClick={() => setOpen(false)}>
              {t("context.close")}
            </Button>
          </div>

          <Show when={failed()}>
            <p role="alert" class="px-3 py-2 text-sm text-danger">
              {t("context.loadFailed")}
            </p>
          </Show>

          <div class="max-h-64 overflow-y-auto p-2">
            <p class="px-1 py-1 text-xs font-medium uppercase tracking-wide text-ink-muted">
              {t("context.tenant")}
            </p>
            <Show
              when={tenants()}
              fallback={
                <p class="px-1 py-1 text-sm text-ink-muted">{t("common.loading")}</p>
              }
            >
              {(loaded) => (
                <Show
                  when={loaded().length > 0}
                  fallback={
                    <p class="px-1 py-1 text-sm text-ink-muted">{t("context.noTenants")}</p>
                  }
                >
                  <ul>
                    <For each={loaded()}>
                      {(tenant) => (
                        <li>
                          <button
                            type="button"
                            onClick={() => chooseTenant(tenant)}
                            class={`flex w-full flex-col rounded-token px-2 py-1 text-left hover:bg-surface-raised ${
                              tenant.tenant_id === tenantId() ? "bg-surface-raised" : ""
                            }`}
                          >
                            <span class="text-sm text-ink">{tenant.name}</span>
                            <span class="font-mono text-xs text-ink-muted">{tenant.tenant_id}</span>
                          </button>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              )}
            </Show>

            <Show when={tenantId()}>
              <p class="mt-3 px-1 py-1 text-xs font-medium uppercase tracking-wide text-ink-muted">
                {t("context.store")}
              </p>
              <Show
                when={stores()}
                fallback={
                  <p class="px-1 py-1 text-sm text-ink-muted">
                    {busy() ? t("common.loading") : t("context.chooseTenantFirst")}
                  </p>
                }
              >
                {(loaded) => (
                  <Show
                    when={loaded().length > 0}
                    fallback={
                      <p class="px-1 py-1 text-sm text-ink-muted">{t("context.noStores")}</p>
                    }
                  >
                    <ul>
                      <For each={loaded()}>
                        {(store) => (
                          <li>
                            <button
                              type="button"
                              onClick={() => chooseStore(store)}
                              class={`flex w-full flex-col rounded-token px-2 py-1 text-left hover:bg-surface-raised ${
                                store.store_id === storeId() ? "bg-surface-raised" : ""
                              }`}
                            >
                              <span class="text-sm text-ink">{store.name}</span>
                              <span class="font-mono text-xs text-ink-muted">{store.store_id}</span>
                            </button>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                )}
              </Show>
            </Show>
          </div>
        </div>
      </Show>
    </div>
  );
}
