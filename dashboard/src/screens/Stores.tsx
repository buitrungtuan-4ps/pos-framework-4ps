// Stores & brands management (ADR-0065, WS-C), on the F2 CRUD kit. The operator's place to give the
// backfilled placeholder stores (`Store 01J9…`) real names, create new stores and brands, and
// archive/restore — all by name, no ULID typed. Tenant-scoped to the picker's context.

import { createSignal, For, Show } from "solid-js";
import { A } from "@solidjs/router";

import { api, ApiError } from "../api/client";
import type { Brand, Store, Tenant } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { screenHref } from "../state/screens";
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
  // The tenant in context, read back from the registry so this screen holds its current name,
  // status, and the version any rename must present (production-readiness O2).
  const [tenant, setTenant] = createSignal<Tenant | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [newStoreName, setNewStoreName] = createSignal("");
  const [newStoreBrand, setNewStoreBrand] = createSignal("");
  const [newBrandName, setNewBrandName] = createSignal("");

  const [editing, setEditing] = createSignal("");
  const [draftName, setDraftName] = createSignal("");
  const [editingBrand, setEditingBrand] = createSignal("");
  const [draftBrandName, setDraftBrandName] = createSignal("");
  const [tenantName, setTenantName] = createSignal("");

  const [pendingArchive, setPendingArchive] = createSignal<Store | null>(null);
  const [pendingBrandArchive, setPendingBrandArchive] = createSignal<Brand | null>(null);
  const [pendingTenantArchive, setPendingTenantArchive] = createSignal<Tenant | null>(null);

  // Errors surface on the page (Banner) and as a transient toast (F1).
  // A `412` means somebody else saved this store while the form was open (ADR-0094). The screen
  // reloads rather than offering a retry: retrying would re-apply the overwrite the refusal exists
  // to prevent, and the operator needs to see what actually changed before deciding again.
  const fail = async (caught: unknown) => {
    if (caught instanceof ApiError && caught.isStale) {
      const message = t("stores.stale");
      setError(message);
      toast.error(message);
      await load();
      return;
    }
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedStores, loadedBrands, loadedTenants] = await Promise.all([
        api.listStores(tenantId()),
        api.listBrands(tenantId()),
        api.listTenants(),
      ]);
      setStores(loadedStores);
      setBrands(loadedBrands);
      // The list is every tenant the super-admin can see; this screen is scoped to one, so it keeps
      // the row for the context and ignores the rest.
      const inContext = loadedTenants.find((row) => row.tenant_id === tenantId()) ?? null;
      setTenant(inContext);
      setTenantName(inContext?.name ?? "");
    } catch (caught) {
      await fail(caught);
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
      await fail(caught);
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
      await fail(caught);
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
      await api.updateStore(
        store.store_id,
        tenantId(),
        { name, status: store.status, brandId: store.brand_id },
        store.etag,
      );
      setEditing("");
      setDraftName("");
      toast.ok(t("stores.renamed"));
      await load();
    } catch (caught) {
      await fail(caught);
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
      await api.updateStore(
        store.store_id,
        tenantId(),
        { name: store.name, status: "archived", brandId: store.brand_id },
        store.etag,
      );
      setPendingArchive(null);
      toast.ok(t("stores.archived"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const restore = async (store: Store) => {
    setError("");
    setBusy(true);
    try {
      await api.updateStore(
        store.store_id,
        tenantId(),
        { name: store.name, status: "active", brandId: store.brand_id },
        store.etag,
      );
      toast.ok(t("stores.restored"));
      await load();
    } catch (caught) {
      await fail(caught);
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
      await api.updateStore(
        store.store_id,
        tenantId(),
        { name: store.name, status: store.status, brandId: brandId || null },
        store.etag,
      );
      toast.ok(t("stores.brandChanged"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- brands (production-readiness O2) --------------------------------------------------------
  // The same three verbs the stores table has had since WS-C, over the `PATCH /admin/brands/{id}`
  // route that shipped with it and had no caller: a brand could be created and never corrected.

  const saveBrandRename = async (brand: Brand) => {
    const name = draftBrandName().trim();
    if (!name) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateBrand(brand.brand_id, tenantId(), { name, status: brand.status }, brand.etag);
      setEditingBrand("");
      setDraftBrandName("");
      toast.ok(t("stores.brandRenamed"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setBrandStatus = async (brand: Brand, status: "active" | "archived") => {
    setError("");
    setBusy(true);
    try {
      await api.updateBrand(brand.brand_id, tenantId(), { name: brand.name, status }, brand.etag);
      setPendingBrandArchive(null);
      toast.ok(status === "archived" ? t("stores.brandArchived") : t("stores.brandRestored"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- the tenant itself ------------------------------------------------------------------------
  // Renaming the organisation in context. Archiving it is offered too, because the route exists and
  // an org that closed should not linger as active — but it is the one action on this screen that
  // changes the context the operator is standing in, so it is behind a confirm that says so.

  const saveTenantRename = async () => {
    const current = tenant();
    const name = tenantName().trim();
    if (!current) {
      return;
    }
    if (!name) {
      setError(t("stores.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateTenant(current.tenant_id, { name, status: current.status }, current.etag);
      toast.ok(t("stores.tenantRenamed"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setTenantStatus = async (status: "active" | "archived") => {
    const current = tenant();
    if (!current) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateTenant(
        current.tenant_id,
        { name: current.name, status },
        current.etag,
      );
      setPendingTenantArchive(null);
      toast.ok(status === "archived" ? t("stores.tenantArchived") : t("stores.tenantRestored"));
      await load();
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const brandColumns = (): Column<Brand>[] => [
    {
      key: "name",
      header: t("stores.brandName"),
      sortValue: (row) => row.name,
      cell: (row) => (
        <Show when={editingBrand() === row.brand_id} fallback={<span>{row.name}</span>}>
          <div class="flex flex-wrap items-center gap-2">
            <input
              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
              aria-label={t("stores.brandName")}
              value={draftBrandName()}
              onInput={(event) => setDraftBrandName(event.currentTarget.value)}
            />
            <Button disabled={busy()} onClick={() => void saveBrandRename(row)}>
              {t("action.save")}
            </Button>
            <Button
              variant="secondary"
              onClick={() => {
                setEditingBrand("");
                setDraftBrandName("");
              }}
            >
              {t("action.cancel")}
            </Button>
          </div>
        </Show>
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
        <TechnicalDetails label={t("common.technicalDetails")}>{row.brand_id}</TechnicalDetails>
      ),
    },
  ];

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
          <Show when={tenant()}>
            {(current) => (
              <Card title={t("stores.organisation")}>
                <div class="flex flex-wrap items-end gap-4">
                  <div class="grow">
                    <TextField
                      label={t("stores.organisationName")}
                      value={tenantName()}
                      onInput={setTenantName}
                    />
                  </div>
                  <StatusBadge
                    tone={current().status === "archived" ? "archived" : "active"}
                    label={
                      current().status === "archived" ? t("status.archived") : t("status.active")
                    }
                  />
                  <Button
                    disabled={busy() || tenantName().trim() === current().name}
                    onClick={() => void saveTenantRename()}
                  >
                    {t("action.save")}
                  </Button>
                  <Show
                    when={current().status === "archived"}
                    fallback={
                      <Button
                        variant="danger"
                        disabled={busy()}
                        onClick={() => setPendingTenantArchive(current())}
                      >
                        {t("stores.archive")}
                      </Button>
                    }
                  >
                    <Button
                      variant="secondary"
                      disabled={busy()}
                      onClick={() => void setTenantStatus("active")}
                    >
                      {t("stores.restore")}
                    </Button>
                  </Show>
                </div>
                <div class="mt-3">
                  <TechnicalDetails label={t("common.technicalDetails")}>
                    {current().tenant_id}
                  </TechnicalDetails>
                </div>
              </Card>
            )}
          </Show>

          <Card
            title={t("stores.list")}
            actions={
              <div class="flex gap-2">
                <A
                  href={screenHref("newStore", tenantId(), "")}
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

          <Card title={t("stores.brands")}>
            <Show
              when={brands().length > 0}
              fallback={<EmptyState title={t("stores.noBrands")} />}
            >
              <DataTable
                columns={brandColumns()}
                rows={brands()}
                searchText={(row) => row.name}
                pageSize={12}
                empty={<EmptyState title={t("stores.noBrands")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex flex-wrap gap-2">
                    <Button
                      variant="secondary"
                      disabled={busy()}
                      onClick={() => {
                        setEditingBrand(row.brand_id);
                        setDraftBrandName(row.name);
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
                          onClick={() => setPendingBrandArchive(row)}
                        >
                          {t("stores.archive")}
                        </Button>
                      }
                    >
                      <Button
                        variant="secondary"
                        disabled={busy()}
                        onClick={() => void setBrandStatus(row, "active")}
                      >
                        {t("stores.restore")}
                      </Button>
                    </Show>
                  </div>
                )}
              />
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
          open={pendingBrandArchive() !== null}
          title={t("stores.brandArchiveTitle")}
          message={t("stores.brandArchiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const brand = pendingBrandArchive();
            if (brand) {
              void setBrandStatus(brand, "archived");
            }
          }}
          onCancel={() => setPendingBrandArchive(null)}
        />

        <ConfirmDialog
          open={pendingTenantArchive() !== null}
          title={t("stores.tenantArchiveTitle")}
          message={t("stores.tenantArchiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void setTenantStatus("archived")}
          onCancel={() => setPendingTenantArchive(null)}
        />

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
