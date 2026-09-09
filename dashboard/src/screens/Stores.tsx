// Stores & brands management (ADR-0065, WS-C), on the authoring kit
// ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)). The operator's place to give
// the backfilled placeholder stores (`Store 01J9…`) real names, create new stores and brands, and
// archive/restore — all by name, no ULID typed. Tenant-scoped to the picker's context.
//
// This is the screen the owner reported: it used to end in two permanently-visible cards, "Create
// store" and "Create brand", occupying the bottom half of the page whether or not anyone wanted to
// create anything. Both are now `FormPanel`s behind an Add button in the header of the list they add
// to, which is Stage 4 item 15 of `docs/cloud-admin-ux-plan.md`, four months after it was recorded
// as delivered.
//
// Three things changed beyond moving the forms, each because the kit made the better shape the
// easy one:
//
//   * **Renaming opens the panel** instead of swapping a raw `<input>` into the table cell. The cell
//     input had no `FormField`, so a refused rename had nowhere to appear.
//   * **A store's brand is part of the same save as its name.** It used to be a `<select>` in the
//     cell that fired a write on `change` — a single mis-click silently reassigned a shop, with no
//     confirmation and no undo.
//   * **Stores and brands hold separate lifecycles**, so saving a brand no longer disables the
//     stores table. That was one shared `busy()` across every button on the screen.

import { createSignal, Show } from "solid-js";
import { A } from "@solidjs/router";

import { api } from "../api/client";
import type { Brand, Store, Tenant } from "../api/types";
import { t } from "../i18n";
import { useEntityCrud } from "../lib/entity-crud";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { screenHref } from "../state/screens";
import {
  Banner,
  Button,
  Card,
  PageHeader,
  SelectField,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormPanel,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage, withStaleReload } from "../lib/errors";

export function Stores() {
  const [stores, setStores] = createSignal<Store[] | null>(null);
  const [brands, setBrands] = createSignal<Brand[]>([]);
  // The tenant in context, read back from the registry so this screen holds its current name,
  // status, and the version any rename must present (production-readiness O2).
  const [tenant, setTenant] = createSignal<Tenant | null>(null);
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);

  // One lifecycle per entity type, not one per screen (ADR-0121 §3). The tenant block gets one too:
  // it never opens a `FormPanel` — a single-record settings row is not a list, and a panel over one
  // field would be ceremony — but it uses `confirming` for the archive and `run` for the rename, so
  // its busy state is its own.
  const storeCrud = useEntityCrud<Store>();
  const brandCrud = useEntityCrud<Brand>();
  const tenantCrud = useEntityCrud<Tenant>();

  // The panel's draft fields. Seeded when a panel opens, read when it submits.
  const [storeName, setStoreName] = createSignal("");
  const [storeBrand, setStoreBrand] = createSignal("");
  const [brandName, setBrandName] = createSignal("");
  const [tenantName, setTenantName] = createSignal("");

  const load = async () => {
    setError("");
    setLoading(true);
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
      const message = apiMessage(caught);
      setError(message);
      toast.error(message);
    } finally {
      setLoading(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  /**
   * Wraps a write so a stale refusal reloads the table and comes back saying so.
   *
   * A `412` means somebody else saved this row while the form was open (ADR-0094). The refusal is
   * re-thrown (reworded) so `crud.run` still keeps the form open with the message beside the
   * refreshed table — which is the whole reason `run` does not close on failure.
   */
  const conditional = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, load, t("stores.stale"));

  /** Reloads and says so, after a write that changed something. */
  const settled = async (message: string) => {
    toast.ok(message);
    await load();
  };

  // --- stores -----------------------------------------------------------------------------------

  const openCreateStore = () => {
    setStoreName("");
    setStoreBrand("");
    storeCrud.create();
  };

  const openEditStore = (row: Store) => {
    setStoreName(row.name);
    setStoreBrand(row.brand_id ?? "");
    storeCrud.edit(row);
  };

  const submitStore = () => {
    const name = storeName().trim();
    if (!name) {
      storeCrud.refuse(t("stores.nameRequired"));
      return;
    }
    const editingRow = storeCrud.subject();
    void storeCrud
      .run(() =>
        conditional(() =>
          editingRow
            ? api.updateStore(
                editingRow.store_id,
                tenantId(),
                { name, status: editingRow.status, brandId: storeBrand() || null },
                editingRow.etag,
              )
            : api.createStore(tenantId(), name, storeBrand() || undefined),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(editingRow ? t("stores.renamed") : t("stores.created"));
        }
      });
  };

  const setStoreStatus = (row: Store, status: "active" | "archived") =>
    storeCrud
      .run(() =>
        conditional(() =>
          api.updateStore(
            row.store_id,
            tenantId(),
            { name: row.name, status, brandId: row.brand_id },
            row.etag,
          ),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(status === "archived" ? t("stores.archived") : t("stores.restored"));
        }
      });

  // --- brands (production-readiness O2) ---------------------------------------------------------
  // The same three verbs the stores table has had since WS-C, over the `PATCH /admin/brands/{id}`
  // route that shipped with it and had no caller: a brand could be created and never corrected.

  const openCreateBrand = () => {
    setBrandName("");
    brandCrud.create();
  };

  const openEditBrand = (row: Brand) => {
    setBrandName(row.name);
    brandCrud.edit(row);
  };

  const submitBrand = () => {
    const name = brandName().trim();
    if (!name) {
      brandCrud.refuse(t("stores.nameRequired"));
      return;
    }
    const editingRow = brandCrud.subject();
    void brandCrud
      .run(() =>
        conditional(() =>
          editingRow
            ? api.updateBrand(
                editingRow.brand_id,
                tenantId(),
                { name, status: editingRow.status },
                editingRow.etag,
              )
            : api.createBrand(tenantId(), name),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(editingRow ? t("stores.brandRenamed") : t("stores.brandCreated"));
        }
      });
  };

  const setBrandStatus = (row: Brand, status: "active" | "archived") =>
    brandCrud
      .run(() =>
        conditional(() =>
          api.updateBrand(row.brand_id, tenantId(), { name: row.name, status }, row.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(status === "archived" ? t("stores.brandArchived") : t("stores.brandRestored"));
        }
      });

  // --- the tenant itself ------------------------------------------------------------------------
  // Renaming the organisation in context. Archiving it is offered too, because the route exists and
  // an org that closed should not linger as active — but it is the one action on this screen that
  // changes the context the operator is standing in, so it is behind a confirm that says so.

  const saveTenantRename = () => {
    const current = tenant();
    const name = tenantName().trim();
    if (!current) {
      return;
    }
    if (!name) {
      tenantCrud.refuse(t("stores.nameRequired"));
      return;
    }
    void tenantCrud
      .run(() =>
        conditional(() =>
          api.updateTenant(current.tenant_id, { name, status: current.status }, current.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(t("stores.tenantRenamed"));
        }
      });
  };

  const setTenantStatus = (status: "active" | "archived") => {
    const current = tenant();
    if (!current) {
      return;
    }
    void tenantCrud
      .run(() =>
        conditional(() =>
          api.updateTenant(current.tenant_id, { name: current.name, status }, current.etag),
        ),
      )
      .then((saved) => {
        if (saved) {
          void settled(status === "archived" ? t("stores.tenantArchived") : t("stores.tenantRestored"));
        }
      });
  };

  /** The brands a store can be assigned to. An archived brand is not offered for a new link. */
  const brandOptions = () =>
    brands()
      .filter((brand) => brand.status !== "archived")
      .map((brand) => ({ value: brand.brand_id, label: brand.name }));

  const brandColumns = (): Column<Brand>[] => [
    {
      key: "name",
      header: t("stores.brandName"),
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
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
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "brand",
      header: t("stores.brand"),
      sortValue: (row) => brands().find((b) => b.brand_id === row.brand_id)?.name ?? "",
      // Read-only now. Reassigning happens in the edit panel, with the name, in one save — the cell
      // used to be a `<select>` that wrote on `change`, so a mis-click moved a shop to another brand
      // with no confirmation.
      cell: (row) => (
        <span class={row.brand_id ? "" : "text-ink-muted"}>
          {brands().find((brand) => brand.brand_id === row.brand_id)?.name ?? t("stores.noBrand")}
        </span>
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
                    disabled={tenantCrud.saving() || tenantName().trim() === current().name}
                    onClick={saveTenantRename}
                  >
                    {tenantCrud.saving() ? t("common.saving") : t("action.save")}
                  </Button>
                  <Show
                    when={current().status === "archived"}
                    fallback={
                      <Button
                        variant="danger"
                        disabled={tenantCrud.saving()}
                        onClick={() => tenantCrud.confirm(current())}
                      >
                        {t("stores.archive")}
                      </Button>
                    }
                  >
                    <Button
                      variant="secondary"
                      disabled={tenantCrud.saving()}
                      onClick={() => void setTenantStatus("active")}
                    >
                      {t("stores.restore")}
                    </Button>
                  </Show>
                </div>
                <Show when={tenantCrud.error()}>
                  {(message) => (
                    <div class="mt-3">
                      <Banner tone="danger" message={message()} />
                    </div>
                  )}
                </Show>
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
                {/* Add sits in the header of the list it adds to (ADR-0121 §6). This screen carries
                    three sections, so the section header is where it belongs; a single button on the
                    `PageHeader` could not say which list it meant. */}
                <Button onClick={openCreateStore}>{t("stores.create")}</Button>
                <A
                  href={screenHref("newStore", tenantId(), "")}
                  class="inline-flex min-h-touch items-center justify-center rounded-token border border-line bg-surface-raised px-4 text-base font-medium text-ink"
                >
                  {t("wizard.open")}
                </A>
                <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
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
                        disabled={storeCrud.saving()}
                        onClick={() => openEditStore(row)}
                      >
                        {t("action.edit")}
                      </Button>
                      <Show
                        when={row.status === "archived"}
                        fallback={
                          <Button
                            variant="danger"
                            disabled={storeCrud.saving()}
                            onClick={() => storeCrud.confirm(row)}
                          >
                            {t("stores.archive")}
                          </Button>
                        }
                      >
                        <Button
                          variant="secondary"
                          disabled={storeCrud.saving()}
                          onClick={() => void setStoreStatus(row, "active")}
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

          <Card
            title={t("stores.brands")}
            actions={<Button onClick={openCreateBrand}>{t("stores.createBrand")}</Button>}
          >
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
                      disabled={brandCrud.saving()}
                      onClick={() => openEditBrand(row)}
                    >
                      {t("action.edit")}
                    </Button>
                    <Show
                      when={row.status === "archived"}
                      fallback={
                        <Button
                          variant="danger"
                          disabled={brandCrud.saving()}
                          onClick={() => brandCrud.confirm(row)}
                        >
                          {t("stores.archive")}
                        </Button>
                      }
                    >
                      <Button
                        variant="secondary"
                        disabled={brandCrud.saving()}
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
        </div>

        <FormPanel
          crud={storeCrud}
          as="modal"
          createTitle={t("stores.create")}
          editTitle={t("stores.edit")}
          submitLabel={storeCrud.mode() === "editing" ? t("action.save") : t("action.create")}
          onSubmit={submitStore}
          dirty={() => storeName().trim() !== (storeCrud.subject()?.name ?? "")}
        >
          <TextField
            label={t("stores.name")}
            value={storeName()}
            onInput={setStoreName}
            placeholder={t("stores.namePlaceholder")}
          />
          <SelectField
            label={t("stores.brand")}
            value={storeBrand()}
            options={brandOptions()}
            onChange={setStoreBrand}
            placeholder={t("stores.noBrand")}
          />
        </FormPanel>

        <FormPanel
          crud={brandCrud}
          as="modal"
          createTitle={t("stores.createBrand")}
          editTitle={t("stores.editBrand")}
          submitLabel={brandCrud.mode() === "editing" ? t("action.save") : t("action.create")}
          onSubmit={submitBrand}
          dirty={() => brandName().trim() !== (brandCrud.subject()?.name ?? "")}
        >
          <TextField
            label={t("stores.brandName")}
            value={brandName()}
            onInput={setBrandName}
            placeholder={t("stores.brandNamePlaceholder")}
          />
        </FormPanel>

        <ConfirmDialog
          open={storeCrud.mode() === "confirming"}
          title={t("stores.archiveTitle")}
          message={t("stores.archiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={storeCrud.saving()}
          onConfirm={() => {
            const row = storeCrud.subject();
            if (row) {
              void setStoreStatus(row, "archived");
            }
          }}
          onCancel={storeCrud.close}
        />

        <ConfirmDialog
          open={brandCrud.mode() === "confirming"}
          title={t("stores.brandArchiveTitle")}
          message={t("stores.brandArchiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={brandCrud.saving()}
          onConfirm={() => {
            const row = brandCrud.subject();
            if (row) {
              void setBrandStatus(row, "archived");
            }
          }}
          onCancel={brandCrud.close}
        />

        <ConfirmDialog
          open={tenantCrud.mode() === "confirming"}
          title={t("stores.tenantArchiveTitle")}
          message={t("stores.tenantArchiveMessage")}
          confirmLabel={t("stores.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={tenantCrud.saving()}
          onConfirm={() => void setTenantStatus("archived")}
          onCancel={tenantCrud.close}
        />
      </RequireContext>
    </div>
  );
}
