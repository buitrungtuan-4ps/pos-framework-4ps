// Stores & brands management (ADR-0065, WS-C). The operator's place to give the backfilled
// placeholder stores (`Store 01J9…`) real names, create new stores and brands, assign a store to a
// brand, and archive/restore — all by name, no ULID typed. Tenant-scoped to the picker's context.

import { createSignal, For, Show } from "solid-js";
import { A } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import type { Brand, Store } from "../api/types";
import { t } from "../i18n";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

export function Stores() {
  const [stores, setStores] = createSignal<Store[] | null>(null);
  const [brands, setBrands] = createSignal<Brand[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [newStoreName, setNewStoreName] = createSignal("");
  const [newStoreBrand, setNewStoreBrand] = createSignal("");
  const [newBrandName, setNewBrandName] = createSignal("");

  const [editing, setEditing] = createSignal("");
  const [draftName, setDraftName] = createSignal("");

  const fail = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : String(caught));

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedStores, loadedBrands] = await Promise.all([
        api.listStores(tenantId()),
        api.listBrands(tenantId()),
      ]);
      setStores(loadedStores);
      setBrands(loadedBrands);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const createStore = async () => {
    const name = newStoreName().trim();
    if (!name) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createStore(tenantId(), name, newStoreBrand() || undefined);
      setNewStoreName("");
      setNewStoreBrand("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const createBrand = async () => {
    const name = newBrandName().trim();
    if (!name) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createBrand(tenantId(), name);
      setNewBrandName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const saveRename = async (store: Store) => {
    const name = draftName().trim();
    if (!name) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateStore(store.store_id, tenantId(), {
        name,
        status: store.status,
        brandId: store.brand_id,
      });
      setEditing("");
      setDraftName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setStoreFields = async (
    store: Store,
    fields: { name?: string; status?: Store["status"]; brandId?: string | null },
  ) => {
    setError("");
    setBusy(true);
    try {
      await api.updateStore(store.store_id, tenantId(), {
        name: fields.name ?? store.name,
        status: fields.status ?? store.status,
        brandId: fields.brandId === undefined ? store.brand_id : fields.brandId,
      });
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("stores.title")} description={t("stores.description")} />
      <Show
        when={tenantId()}
        fallback={<Banner tone="danger" message={t("context.tenantRequired")} />}
      >
        <div class="flex flex-col gap-6">
          <Card
            title={t("stores.list")}
            actions={
              <div class="flex gap-2">
                <A
                  href="/stores/new"
                  class="inline-flex min-h-touch items-center justify-center rounded-token bg-accent px-4 text-base font-medium text-accent-ink"
                >
                  {t("wizard.open")}
                </A>
                <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                  {t("action.load")}
                </Button>
              </div>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show
              when={stores()}
              fallback={<p class="text-sm text-ink-muted">{t("stores.loadHint")}</p>}
            >
              {(loaded) => (
                <Show
                  when={loaded().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("stores.empty")}</p>}
                >
                  <div class="overflow-x-auto">
                    <table class="w-full text-left text-sm">
                      <thead>
                        <tr class="border-b border-line text-ink-muted">
                          <th class="py-2 pr-4 font-medium">{t("stores.name")}</th>
                          <th class="py-2 pr-4 font-medium">{t("stores.brand")}</th>
                          <th class="py-2 pr-4 font-medium">{t("stores.status")}</th>
                          <th class="py-2 font-medium">{t("stores.actions")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={loaded()}>
                          {(store) => (
                            <tr class="border-b border-line align-top text-ink">
                              <td class="py-2 pr-4">
                                <Show
                                  when={editing() === store.store_id}
                                  fallback={
                                    <div class="flex flex-col">
                                      <span>{store.name}</span>
                                      <span class="font-mono text-xs text-ink-muted">
                                        {store.store_id}
                                      </span>
                                    </div>
                                  }
                                >
                                  <div class="flex flex-wrap items-center gap-2">
                                    <input
                                      class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                      aria-label={t("stores.name")}
                                      value={draftName()}
                                      onInput={(event) => setDraftName(event.currentTarget.value)}
                                    />
                                    <Button disabled={busy()} onClick={() => void saveRename(store)}>
                                      {t("action.save")}
                                    </Button>
                                    <Button
                                      variant="secondary"
                                      onClick={() => {
                                        setEditing("");
                                        setDraftName("");
                                      }}
                                    >
                                      {t("action.cancel")}
                                    </Button>
                                  </div>
                                </Show>
                              </td>
                              <td class="py-2 pr-4">
                                <select
                                  class="min-h-touch rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                  aria-label={t("stores.brand")}
                                  value={store.brand_id ?? ""}
                                  onChange={(event) =>
                                    void setStoreFields(store, {
                                      brandId: event.currentTarget.value || null,
                                    })
                                  }
                                >
                                  <option value="">{t("stores.noBrand")}</option>
                                  <For each={brands()}>
                                    {(brand) => <option value={brand.brand_id}>{brand.name}</option>}
                                  </For>
                                </select>
                              </td>
                              <td class="py-2 pr-4">
                                {store.status === "archived"
                                  ? t("stores.statusArchived")
                                  : t("stores.statusActive")}
                              </td>
                              <td class="flex flex-wrap gap-2 py-2">
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditing(store.store_id);
                                    setDraftName(store.name);
                                  }}
                                >
                                  {t("stores.rename")}
                                </Button>
                                <Button
                                  variant={store.status === "archived" ? "secondary" : "danger"}
                                  disabled={busy()}
                                  onClick={() =>
                                    void setStoreFields(store, {
                                      status: store.status === "archived" ? "active" : "archived",
                                    })
                                  }
                                >
                                  {store.status === "archived"
                                    ? t("stores.restore")
                                    : t("stores.archive")}
                                </Button>
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              )}
            </Show>
          </Card>

          <div class="grid gap-6 lg:grid-cols-2">
            <Card title={t("stores.create")}>
              <div class="flex flex-col gap-4">
                <TextField
                  label={t("stores.name")}
                  value={newStoreName()}
                  onInput={setNewStoreName}
                  placeholder={t("stores.namePlaceholder")}
                />
                <label class="block">
                  <span class="mb-1 block text-sm font-medium text-ink">{t("stores.brand")}</span>
                  <select
                    class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                    value={newStoreBrand()}
                    onChange={(event) => setNewStoreBrand(event.currentTarget.value)}
                  >
                    <option value="">{t("stores.noBrand")}</option>
                    <For each={brands()}>
                      {(brand) => <option value={brand.brand_id}>{brand.name}</option>}
                    </For>
                  </select>
                </label>
                <Button disabled={busy()} onClick={() => void createStore()}>
                  {t("action.create")}
                </Button>
              </div>
            </Card>

            <Card title={t("stores.createBrand")}>
              <div class="flex flex-col gap-4">
                <TextField
                  label={t("stores.brandName")}
                  value={newBrandName()}
                  onInput={setNewBrandName}
                  placeholder={t("stores.brandNamePlaceholder")}
                />
                <Button disabled={busy()} onClick={() => void createBrand()}>
                  {t("action.create")}
                </Button>
              </div>
            </Card>
          </div>
        </div>
      </Show>
    </div>
  );
}
