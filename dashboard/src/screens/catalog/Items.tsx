// The Items sub-screen (ADR-0082, Track F3): the product master on the F2 CRUD kit. Behaviour is
// preserved from the monolith — create (name + required tax class + optional taxonomy), rename with
// per-locale names (ADR-0074), the inline image widget (ADR-0075), archive/restore, and the
// owner/admin CSV export — but rendered as a searchable `DataTable` with the ULID behind a
// `TechnicalDetails` disclosure, a `Drawer` for create and for edit, and the shared `StatusCell`.
// Tax class, category and sub-category are set at create and preserved on every edit, exactly as the
// monolith did (a rename or a status flip re-sends the item's existing taxonomy untouched).

import { createSignal, For, Show } from "solid-js";

import { api } from "../../api/client";
import type { CatalogItem, ItemCategory, ItemSubcategory, TaxClass } from "../../api/types";
import { LOCALES, localeName, t } from "../../i18n";
import { onScopedContext } from "../../lib/scoped";
import { actingAdmin, tenantId } from "../../state/session";
import { Banner, Button, Card, TextField } from "../../components/ui";
import { type Column, DataTable, Drawer, EmptyState, FormField, TechnicalDetails } from "../../components/kit";
import { toast } from "../../components/Toast";
import { ImagePicker } from "../../components/ImagePicker";
import { cleanTranslations, errorMessage, StatusCell } from "./shared";

export function CatalogItems() {
  const [items, setItems] = createSignal<CatalogItem[] | null>(null);
  const [taxClasses, setTaxClasses] = createSignal<TaxClass[]>([]);
  const [categories, setCategories] = createSignal<ItemCategory[]>([]);
  const [subcategories, setSubcategories] = createSignal<ItemSubcategory[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Create drawer.
  const [creating, setCreating] = createSignal(false);
  const [newName, setNewName] = createSignal("");
  const [newTaxClass, setNewTaxClass] = createSignal("");
  const [newCategory, setNewCategory] = createSignal("");
  const [newSubcategory, setNewSubcategory] = createSignal("");

  // Edit drawer — the item being edited, its draft name, and its per-locale names (ADR-0074).
  const [editing, setEditing] = createSignal<CatalogItem | null>(null);
  const [draftName, setDraftName] = createSignal("");
  const [draftTranslations, setDraftTranslations] = createSignal<Record<string, string>>({});

  const taxClassName = (id: string) =>
    taxClasses().find((row) => row.tax_class_id === id)?.name ?? id;
  const categoryName = (id: string | null) =>
    id ? (categories().find((row) => row.item_category_id === id)?.name ?? id) : "—";

  // console.media.manage → owner/admin (the server re-checks). Gates the per-item image widget's write
  // affordances and, since the same role set holds console.catalog.manage, the CSV export button.
  const canManageMedia = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };

  const activeTaxClasses = () => taxClasses().filter((row) => row.status === "active");
  const activeCategories = () => categories().filter((row) => row.status === "active");
  const activeSubcategories = () =>
    subcategories().filter((row) => row.status === "active" && row.item_category_id === newCategory());

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedItems, loadedTaxClasses, loadedCategories, loadedSubcategories] = await Promise.all([
        api.listItems(tenantId()),
        api.listTaxClasses(tenantId()),
        api.listItemCategories(tenantId()),
        api.listItemSubcategories(tenantId()),
      ]);
      setItems(loadedItems);
      setTaxClasses(loadedTaxClasses);
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

  const openCreate = () => {
    setNewName("");
    setNewTaxClass("");
    setNewCategory("");
    setNewSubcategory("");
    setCreating(true);
  };

  const createItem = async () => {
    const name = newName().trim();
    const taxClass = newTaxClass().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    if (!taxClass) {
      toast.error(t("catalog.taxClassRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createItem(tenantId(), name, taxClass, {
        itemCategoryId: newCategory() || null,
        itemSubcategoryId: newSubcategory() || null,
      });
      toast.ok(t("catalog.itemCreated"));
      setCreating(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // The one write path: PATCH the item with a single changed facet, preserving the taxonomy and the
  // fields not being edited (exactly the monolith's `setItemFields`). Returns whether it succeeded so
  // the caller can toast the right message and close its drawer.
  const applyItem = async (
    item: CatalogItem,
    fields: {
      name?: string;
      nameTranslations?: Record<string, string>;
      status?: "active" | "archived";
      imageRef?: string | null;
    },
  ): Promise<boolean> => {
    const name = (fields.name ?? item.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateItem(item.menu_item_id, tenantId(), {
        name,
        nameTranslations: cleanTranslations(fields.nameTranslations ?? item.name_translations),
        taxClassId: item.tax_class_id,
        itemCategoryId: item.item_category_id,
        itemSubcategoryId: item.item_subcategory_id,
        imageRef: fields.imageRef !== undefined ? fields.imageRef : item.image_ref,
        status: fields.status ?? item.status,
      });
      await load();
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openEdit = (item: CatalogItem) => {
    setEditing(item);
    setDraftName(item.name);
    setDraftTranslations({ ...item.name_translations });
  };

  const saveEdit = async () => {
    const item = editing();
    if (!item) {
      return;
    }
    const ok = await applyItem(item, { name: draftName(), nameTranslations: draftTranslations() });
    if (ok) {
      toast.ok(t("catalog.itemSaved"));
      setEditing(null);
    }
  };

  const toggleArchive = async (item: CatalogItem) => {
    const archiving = item.status !== "archived";
    const ok = await applyItem(item, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.itemArchived") : t("catalog.itemRestored"));
    }
  };

  const setImage = async (item: CatalogItem, mediaId: string | null) => {
    const ok = await applyItem(item, { imageRef: mediaId });
    if (ok) {
      toast.ok(t("catalog.itemSaved"));
    }
  };

  const exportItems = async () => {
    try {
      await api.exportItemsCsv(tenantId());
    } catch (caught) {
      toast.error(errorMessage(caught));
    }
  };

  const columns = (): Column<CatalogItem>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => (
        <div class="flex flex-col gap-1">
          <span>{row.name}</span>
          <TechnicalDetails label={t("common.technicalDetails")}>
            <div>{row.menu_item_id}</div>
          </TechnicalDetails>
        </div>
      ),
    },
    {
      key: "image",
      header: t("catalog.image"),
      cell: (row) => (
        <ImagePicker
          tenantId={tenantId()}
          value={row.image_ref}
          canManage={canManageMedia()}
          disabled={busy()}
          onChange={(mediaId) => void setImage(row, mediaId)}
        />
      ),
    },
    {
      key: "taxClass",
      header: t("catalog.taxClass"),
      sortValue: (row) => taxClassName(row.tax_class_id),
      cell: (row) => taxClassName(row.tax_class_id),
    },
    {
      key: "category",
      header: t("catalog.category"),
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
        title={t("catalog.items")}
        actions={
          <div class="flex flex-wrap gap-2">
            <Show when={canManageMedia()}>
              <Button variant="secondary" disabled={busy()} onClick={() => void exportItems()}>
                {t("catalog.exportCsv")}
              </Button>
            </Show>
            <Button disabled={busy()} onClick={openCreate}>
              {t("catalog.createItem")}
            </Button>
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          </div>
        }
      >
        <Show
          when={items()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              searchText={(row) => `${row.name} ${taxClassName(row.tax_class_id)}`}
              pageSize={12}
              empty={<EmptyState title={t("catalog.itemsEmpty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <div class="flex flex-wrap gap-2">
                  <Button variant="secondary" disabled={busy()} onClick={() => openEdit(row)}>
                    {t("action.edit")}
                  </Button>
                  <Button
                    variant={row.status === "archived" ? "secondary" : "danger"}
                    disabled={busy()}
                    onClick={() => void toggleArchive(row)}
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
        open={creating()}
        title={t("catalog.createItem")}
        closeLabel={t("action.close")}
        onClose={() => setCreating(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreating(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createItem()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <TextField
            label={t("catalog.name")}
            value={newName()}
            onInput={setNewName}
            placeholder={t("catalog.namePlaceholder")}
          />
          <FormField label={t("catalog.taxClass")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={newTaxClass()}
              onChange={(event) => setNewTaxClass(event.currentTarget.value)}
            >
              <option value="">{t("catalog.chooseTaxClass")}</option>
              <For each={activeTaxClasses()}>
                {(row) => <option value={row.tax_class_id}>{row.name}</option>}
              </For>
            </select>
            <Show when={activeTaxClasses().length === 0}>
              <p class="mt-1 text-xs text-ink-muted">{t("catalog.taxClassEmpty")}</p>
            </Show>
          </FormField>
          <FormField label={t("catalog.category")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={newCategory()}
              onChange={(event) => {
                setNewCategory(event.currentTarget.value);
                setNewSubcategory("");
              }}
            >
              <option value="">{t("catalog.noCategory")}</option>
              <For each={activeCategories()}>
                {(row) => <option value={row.item_category_id}>{row.name}</option>}
              </For>
            </select>
          </FormField>
          <FormField label={t("catalog.subcategory")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-50"
              value={newSubcategory()}
              disabled={!newCategory()}
              onChange={(event) => setNewSubcategory(event.currentTarget.value)}
            >
              <option value="">{t("catalog.noSubcategory")}</option>
              <For each={activeSubcategories()}>
                {(row) => <option value={row.item_subcategory_id}>{row.name}</option>}
              </For>
            </select>
          </FormField>
        </div>
      </Drawer>

      <Drawer
        open={editing() !== null}
        title={editing()?.name ?? t("action.edit")}
        closeLabel={t("action.close")}
        onClose={() => setEditing(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditing(null)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void saveEdit()}>
              {t("action.save")}
            </Button>
          </>
        }
      >
        <Show when={editing()}>
          <div class="flex flex-col gap-4">
            <TextField label={t("catalog.name")} value={draftName()} onInput={setDraftName} />
            <div>
              <p class="mb-2 text-xs text-ink-muted">{t("catalog.localizedNamesHint")}</p>
              <div class="flex flex-col gap-2">
                <For each={LOCALES}>
                  {(code) => (
                    <label class="flex items-center gap-2 text-sm">
                      <span class="w-24 shrink-0 text-ink-muted">{localeName(code)}</span>
                      <input
                        class="min-h-touch flex-1 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                        aria-label={localeName(code)}
                        value={draftTranslations()[code] ?? ""}
                        onInput={(event) =>
                          setDraftTranslations({
                            ...draftTranslations(),
                            [code]: event.currentTarget.value,
                          })
                        }
                      />
                    </label>
                  )}
                </For>
              </div>
            </div>
          </div>
        </Show>
      </Drawer>
    </div>
  );
}
