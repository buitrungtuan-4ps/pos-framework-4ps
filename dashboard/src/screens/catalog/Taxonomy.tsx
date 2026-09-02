// The Taxonomy sub-screen (ADR-0082, Track F3): the operational taxonomy — item categories and the
// sub-categories nested under them (ADR-0066 entities 2/3) — on the F2 CRUD kit. Behaviour preserved
// from the monolith: create a category by name; create a sub-category under a required parent
// category; rename either; archive/restore either. Two kit `DataTable`s, one per level, with create
// and rename in a `Drawer` and status through the shared `StatusCell`. A sub-category's parent is
// chosen at create and carried through on edit, exactly as the monolith did (rename does not move it).

import { createSignal, For, Show } from "solid-js";

import { api } from "../../api/client";
import type { ItemCategory, ItemSubcategory } from "../../api/types";
import { t } from "../../i18n";
import { onScopedContext } from "../../lib/scoped";
import { tenantId } from "../../state/session";
import { Banner, Button, Card, TextField } from "../../components/ui";
import { type Column, DataTable, Drawer, EmptyState, FormField } from "../../components/kit";
import { toast } from "../../components/Toast";
import { errorMessage, isStale, StatusCell } from "./shared";

export function CatalogTaxonomy() {
  const [categories, setCategories] = createSignal<ItemCategory[] | null>(null);
  const [subcategories, setSubcategories] = createSignal<ItemSubcategory[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Category create/edit drawers.
  const [creatingCategory, setCreatingCategory] = createSignal(false);
  const [newCategoryName, setNewCategoryName] = createSignal("");
  const [editingCategory, setEditingCategory] = createSignal<ItemCategory | null>(null);

  // Sub-category create/edit drawers.
  const [creatingSubcategory, setCreatingSubcategory] = createSignal(false);
  const [newSubcategoryName, setNewSubcategoryName] = createSignal("");
  const [newSubcategoryParent, setNewSubcategoryParent] = createSignal("");
  const [editingSubcategory, setEditingSubcategory] = createSignal<ItemSubcategory | null>(null);

  // The single draft-name signal both edit drawers write to (only one is open at a time).
  const [draftName, setDraftName] = createSignal("");

  const categoryName = (id: string) =>
    (categories() ?? []).find((row) => row.item_category_id === id)?.name ?? id;
  const activeCategories = () => (categories() ?? []).filter((row) => row.status === "active");

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedCategories, loadedSubcategories] = await Promise.all([
        api.listItemCategories(tenantId()),
        api.listItemSubcategories(tenantId()),
      ]);
      setCategories(loadedCategories);
      setSubcategories(loadedSubcategories);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  // --- categories ---

  const openCreateCategory = () => {
    setNewCategoryName("");
    setCreatingCategory(true);
  };

  const createCategory = async () => {
    const name = newCategoryName().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createItemCategory(tenantId(), name);
      toast.ok(t("catalog.categoryCreated"));
      setCreatingCategory(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applyCategory = async (
    row: ItemCategory,
    fields: { name?: string; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateItemCategory(row.item_category_id, tenantId(), {
        name,
        status: fields.status ?? row.status,
      }, row.etag);
      await load();
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      // A stale copy is recovered by reloading, so the reader sees what actually changed.
      if (isStale(caught)) {
        await load();
      }
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openEditCategory = (row: ItemCategory) => {
    setEditingCategory(row);
    setDraftName(row.name);
  };

  const saveCategory = async () => {
    const row = editingCategory();
    if (!row) {
      return;
    }
    const ok = await applyCategory(row, { name: draftName() });
    if (ok) {
      toast.ok(t("catalog.categorySaved"));
      setEditingCategory(null);
    }
  };

  const toggleCategory = async (row: ItemCategory) => {
    const archiving = row.status !== "archived";
    const ok = await applyCategory(row, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.categoryArchived") : t("catalog.categoryRestored"));
    }
  };

  // --- sub-categories ---

  const openCreateSubcategory = () => {
    setNewSubcategoryName("");
    setNewSubcategoryParent("");
    setCreatingSubcategory(true);
  };

  const createSubcategory = async () => {
    const name = newSubcategoryName().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    if (!newSubcategoryParent()) {
      toast.error(t("catalog.parentCategoryRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createItemSubcategory(tenantId(), newSubcategoryParent(), name);
      toast.ok(t("catalog.subcategoryCreated"));
      setCreatingSubcategory(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applySubcategory = async (
    row: ItemSubcategory,
    fields: { name?: string; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateItemSubcategory(row.item_subcategory_id, tenantId(), {
        itemCategoryId: row.item_category_id,
        name,
        status: fields.status ?? row.status,
      }, row.etag);
      await load();
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      // A stale copy is recovered by reloading, so the reader sees what actually changed.
      if (isStale(caught)) {
        await load();
      }
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openEditSubcategory = (row: ItemSubcategory) => {
    setEditingSubcategory(row);
    setDraftName(row.name);
  };

  const saveSubcategory = async () => {
    const row = editingSubcategory();
    if (!row) {
      return;
    }
    const ok = await applySubcategory(row, { name: draftName() });
    if (ok) {
      toast.ok(t("catalog.subcategorySaved"));
      setEditingSubcategory(null);
    }
  };

  const toggleSubcategory = async (row: ItemSubcategory) => {
    const archiving = row.status !== "archived";
    const ok = await applySubcategory(row, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.subcategoryArchived") : t("catalog.subcategoryRestored"));
    }
  };

  const categoryColumns = (): Column<ItemCategory>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => row.name,
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  const subcategoryColumns = (): Column<ItemSubcategory>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => row.name,
    },
    {
      key: "parent",
      header: t("catalog.parentCategory"),
      sortValue: (row) => categoryName(row.item_category_id),
      cell: (row) => categoryName(row.item_category_id),
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  return (
    <div class="flex flex-col gap-6">
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

      <Card
        title={t("catalog.categories")}
        actions={
          <div class="flex flex-wrap gap-2">
            <Button disabled={busy()} onClick={openCreateCategory}>
              {t("action.create")}
            </Button>
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          </div>
        }
      >
        <Show
          when={categories()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={categoryColumns()}
              rows={loaded()}
              searchText={(row) => row.name}
              pageSize={12}
              empty={<EmptyState title={t("catalog.categoriesEmpty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <div class="flex flex-wrap gap-2">
                  <Button variant="secondary" disabled={busy()} onClick={() => openEditCategory(row)}>
                    {t("action.edit")}
                  </Button>
                  <Button
                    variant={row.status === "archived" ? "secondary" : "danger"}
                    disabled={busy()}
                    onClick={() => void toggleCategory(row)}
                  >
                    {row.status === "archived" ? t("catalog.restore") : t("catalog.archive")}
                  </Button>
                </div>
              )}
            />
          )}
        </Show>
      </Card>

      <Card
        title={t("catalog.subcategories")}
        actions={
          <Button disabled={busy()} onClick={openCreateSubcategory}>
            {t("action.create")}
          </Button>
        }
      >
        <Show
          when={subcategories()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={subcategoryColumns()}
              rows={loaded()}
              searchText={(row) => `${row.name} ${categoryName(row.item_category_id)}`}
              pageSize={12}
              empty={<EmptyState title={t("catalog.subcategoriesEmpty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <div class="flex flex-wrap gap-2">
                  <Button
                    variant="secondary"
                    disabled={busy()}
                    onClick={() => openEditSubcategory(row)}
                  >
                    {t("action.edit")}
                  </Button>
                  <Button
                    variant={row.status === "archived" ? "secondary" : "danger"}
                    disabled={busy()}
                    onClick={() => void toggleSubcategory(row)}
                  >
                    {row.status === "archived" ? t("catalog.restore") : t("catalog.archive")}
                  </Button>
                </div>
              )}
            />
          )}
        </Show>
      </Card>

      <Drawer
        open={creatingCategory()}
        title={t("catalog.categoryName")}
        closeLabel={t("action.close")}
        onClose={() => setCreatingCategory(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreatingCategory(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createCategory()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <TextField
          label={t("catalog.categoryName")}
          value={newCategoryName()}
          onInput={setNewCategoryName}
          placeholder={t("catalog.categoryNamePlaceholder")}
        />
      </Drawer>

      <Drawer
        open={editingCategory() !== null}
        title={editingCategory()?.name ?? t("action.edit")}
        closeLabel={t("action.close")}
        onClose={() => setEditingCategory(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditingCategory(null)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void saveCategory()}>
              {t("action.save")}
            </Button>
          </>
        }
      >
        <TextField label={t("catalog.name")} value={draftName()} onInput={setDraftName} />
      </Drawer>

      <Drawer
        open={creatingSubcategory()}
        title={t("catalog.subcategoryName")}
        closeLabel={t("action.close")}
        onClose={() => setCreatingSubcategory(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreatingSubcategory(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createSubcategory()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <FormField label={t("catalog.parentCategory")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={newSubcategoryParent()}
              onChange={(event) => setNewSubcategoryParent(event.currentTarget.value)}
            >
              <option value="">{t("catalog.chooseCategory")}</option>
              <For each={activeCategories()}>
                {(row) => <option value={row.item_category_id}>{row.name}</option>}
              </For>
            </select>
          </FormField>
          <TextField
            label={t("catalog.subcategoryName")}
            value={newSubcategoryName()}
            onInput={setNewSubcategoryName}
            placeholder={t("catalog.subcategoryNamePlaceholder")}
          />
        </div>
      </Drawer>

      <Drawer
        open={editingSubcategory() !== null}
        title={editingSubcategory()?.name ?? t("action.edit")}
        closeLabel={t("action.close")}
        onClose={() => setEditingSubcategory(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditingSubcategory(null)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void saveSubcategory()}>
              {t("action.save")}
            </Button>
          </>
        }
      >
        <TextField label={t("catalog.name")} value={draftName()} onInput={setDraftName} />
      </Drawer>
    </div>
  );
}
