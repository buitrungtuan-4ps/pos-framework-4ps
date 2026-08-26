// Stores & brands management (ADR-0065, WS-C), on the F2 CRUD kit. The operator's place to give the
// backfilled placeholder stores (`Store 01J9…`) real names, create new stores and brands, and
// archive/restore — all by name, no ULID typed. Tenant-scoped to the picker's context.

import { createSignal, For, Show } from "solid-js";
import { A } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import type { Brand, Store } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

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

  const [pendingArchive, setPendingArchive] = createSignal<Store | null>(null);

  // Errors surface on the page (Banner) and as a transient toast (F1).
  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

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

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

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
      toast.ok(t("stores.created"));
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
      toast.ok(t("stores.brandCreated"));
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
      toast.ok(t("stores.renamed"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const archive = async () => {
    const store = pendingArchive();
    if (!store) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateStore(store.store_id, tenantId(), {
        name: store.name,
        status: "archived",
        brandId: store.brand_id,
      });
      setPendingArchive(null);
      toast.ok(t("stores.archived"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const restore = async (store: Store) => {
    setError("");
    setBusy(true);
    try {
      await api.updateStore(store.store_id, tenantId(), {
        name: store.name,
        status: "active",
        brandId: store.brand_id,
      });
      toast.ok(t("stores.restored"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Reassign a store to a different brand (or none) in place — the store's other fields are carried
  // through unchanged, so this only moves the brand link.
  const reassignBrand = async (store: Store, brandId: string) => {
    setError("");
    setBusy(true);
    try {
      await api.updateStore(store.store_id, tenantId(), {
        name: store.name,
        status: store.status,
        brandId: brandId || null,
      });
      toast.ok(t("stores.brandChanged"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<Store>[] => [
    {
      key: "name",
      header: t("stores.name"),
      sortValue: (row) => row.name,
      cell: (row) => (
        <Show when={editing() === row.store_id} fallback={<span>{row.name}</span>}>
          <div class="flex flex-wrap items-center gap-2">
            <input
              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
              aria-label={t("stores.name")}
              value={draftName()}
              onInput={(event) => setDraftName(event.currentTarget.value)}
            />
            <Button disabled={busy()} onClick={() => void saveRename(row)}>
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
      ),
    },
    {
      key: "brand",
      header: t("stores.brand"),
      cell: (row) => (
        <select
          class="min-h-touch rounded-token border border-line bg-surface-raised px-2 text-sm text-ink disabled:opacity-60"
          aria-label={t("stores.brand")}
          disabled={busy() || row.status === "archived"}
          value={row.brand_id ?? ""}
          onChange={(event) => void reassignBrand(row, event.currentTarget.value)}
        >
          <option value="">{t("stores.noBrand")}</option>
          <For each={brands()}>
            {(brand) => <option value={brand.brand_id}>{brand.name}</option>}
          </For>
        </select>
      ),
    },
    {
      key: "status",
      header: t("stores.status"),
      cell: (row) => (
        <StatusBadge
          tone={row.status === "archived" ? "archived" : "active"}
          label={row.status === "archived" ? t("status.archived") : t("status.active")}
        />
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.store_id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("stores.title")} description={t("stores.description")} />
      <RequireContext need="tenant">
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
                  {t("action.refresh")}
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
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={<EmptyState title={t("stores.empty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <div class="flex flex-wrap gap-2">
                      <Button
                        variant="secondary"
                        disabled={busy()}
                        onClick={() => {
                          setEditing(row.store_id);
                          setDraftName(row.name);
                        }}
                      >
                        {t("stores.rename")}
                      </Button>
                      <Show
                        when={row.status === "archived"}
                        fallback={
                          <Button
                            variant="danger"
                            disabled={busy()}
                            onClick={() => setPendingArchive(row)}
                          >
                            {t("stores.archive")}
                          </Button>
                        }
                      >
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => void restore(row)}
                        >
                          {t("stores.restore")}
                        </Button>
                      </Show>
                    </div>
                  )}
                />
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

        <ConfirmDialog
          open={pendingArchive() !== null}
          title={t("stores.archiveTitle")}
          message={t("stores.archiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void archive()}
          onCancel={() => setPendingArchive(null)}
        />
      </RequireContext>
    </div>
  );
}
