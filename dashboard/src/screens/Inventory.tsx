// The Inventory authoring screen (ADR-0079, Track M6), on the F2 CRUD kit. An operator authors the
// tenant's ingredients (name + unit), per-item recipes (a bill of materials — ingredient lines with a
// per-unit amount — plus an auto-86 threshold), and lightweight supplier references, then publishes
// the composed `inventory` node to a store's config so the edge builds its RecipeBook and thresholds.
// Recipes are keyed by the menu item they make (chosen from the tenant's catalog); ingredients and
// suppliers carry a server-minted id. Adding is a create that refuses a key already taken and
// editing is conditional on the version the row was read at (ADR-0095), so two operators cannot
// silently overwrite each other's work. Publishing needs a store chosen in
// the top bar. Recipe amounts are proprietary process (T2): they live here, never in the audit trail.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import {
  UNITS,
  type CatalogItem,
  type ETag,
  type Ingredient,
  type IngredientInput,
  type Recipe,
  type RecipeInput,
  type RecipeLine,
  type Supplier,
  type SupplierInput,
  type UnitOfMeasure,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/** The unit tokens mapped to their labels. */
const UNIT_LABEL: Record<UnitOfMeasure, MessageKey> = {
  UNIT_OF_MEASURE_GRAM: "inventory.unit.gram",
  UNIT_OF_MEASURE_KILOGRAM: "inventory.unit.kilogram",
  UNIT_OF_MEASURE_MILLILITER: "inventory.unit.milliliter",
  UNIT_OF_MEASURE_LITER: "inventory.unit.liter",
  UNIT_OF_MEASURE_PIECE: "inventory.unit.piece",
};

/** A `Quantity` (thousandths of a unit) as a plain amount string: `{ milli: 100000 }` → "100". */
function milliToAmount(milli: number): string {
  return String(milli / 1000);
}

/** A whole-or-decimal amount string as thousandths, or `null` when malformed or not positive. */
function amountToMilli(text: string): number | null {
  const value = Number(text.trim());
  if (!Number.isFinite(value) || value <= 0) {
    return null;
  }
  return Math.round(value * 1000);
}

/** One editable BOM line in the recipe drawer: an ingredient id and its per-unit amount as text. */
type LineDraft = { ingredient: string; amount: string };

export function Inventory() {
  const [ingredients, setIngredients] = createSignal<Ingredient[] | null>(null);
  const [recipes, setRecipes] = createSignal<Recipe[]>([]);
  const [suppliers, setSuppliers] = createSignal<Supplier[]>([]);
  const [items, setItems] = createSignal<CatalogItem[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Ingredient drawer.
  const [ingOpen, setIngOpen] = createSignal(false);
  // Each editing target is the key *and* the version the row was read at: an update is conditional
  // on that version (ADR-0095), so the two travel together rather than in signals that could drift.
  const [ingEditing, setIngEditing] = createSignal<{ id: string; etag: ETag } | null>(null);
  const [ingName, setIngName] = createSignal("");
  const [ingUnit, setIngUnit] = createSignal<UnitOfMeasure>("UNIT_OF_MEASURE_GRAM");
  const [pendingIngDelete, setPendingIngDelete] = createSignal<Ingredient | null>(null);

  // Recipe drawer (keyed by item; the item is chosen on create, fixed on edit).
  const [recOpen, setRecOpen] = createSignal(false);
  const [recEditing, setRecEditing] = createSignal<{ item: string; etag: ETag } | null>(null);
  const [recItem, setRecItem] = createSignal("");
  const [recThreshold, setRecThreshold] = createSignal("0");
  const [recLines, setRecLines] = createSignal<LineDraft[]>([]);
  const [pendingRecDelete, setPendingRecDelete] = createSignal<Recipe | null>(null);

  // Supplier drawer.
  const [supOpen, setSupOpen] = createSignal(false);
  const [supEditing, setSupEditing] = createSignal<{ id: string; etag: ETag } | null>(null);
  const [supName, setSupName] = createSignal("");
  const [pendingSupDelete, setPendingSupDelete] = createSignal<Supplier | null>(null);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [ing, rec, sup, cat] = await Promise.all([
        api.listIngredients(tenantId()),
        api.listRecipes(tenantId()),
        api.listSuppliers(tenantId()),
        api.listItems(tenantId()),
      ]);
      setIngredients(ing);
      setRecipes(rec);
      setSuppliers(sup);
      setItems(cat);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant or store changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  /** The name of a menu item by id, falling back to the id when the catalog has no such item. */
  const itemName = (id: string): string => items().find((i) => i.menu_item_id === id)?.name ?? id;

  // --- Ingredients ---

  const openIngCreate = () => {
    setIngEditing(null);
    setIngName("");
    setIngUnit("UNIT_OF_MEASURE_GRAM");
    setIngOpen(true);
  };

  const openIngEdit = (row: Ingredient) => {
    setIngEditing({ id: row.id, etag: row.etag });
    setIngName(row.name);
    setIngUnit(row.unit);
    setIngOpen(true);
  };

  const saveIngredient = async () => {
    const name = ingName().trim();
    if (!name) {
      setError(t("inventory.nameRequired"));
      return;
    }
    const input: IngredientInput = { name, unit: ingUnit() };
    setBusy(true);
    try {
      const target = ingEditing();
      if (target) {
        await api.updateIngredient(tenantId(), target.id, target.etag, input);
        toast.ok(t("inventory.ingredientSaved"));
      } else {
        await api.createIngredient(tenantId(), input);
        toast.ok(t("inventory.ingredientSaved"));
      }
      setIngOpen(false);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removeIngredient = async () => {
    const row = pendingIngDelete();
    if (!row) {
      return;
    }
    setBusy(true);
    try {
      await api.deleteIngredient(tenantId(), row.id);
      setPendingIngDelete(null);
      toast.ok(t("inventory.ingredientDeleted"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- Recipes ---

  const openRecCreate = () => {
    setRecEditing(null);
    setRecItem(items()[0]?.menu_item_id ?? "");
    setRecThreshold("0");
    setRecLines([]);
    setRecOpen(true);
  };

  const openRecEdit = (row: Recipe) => {
    setRecEditing({ item: row.item, etag: row.etag });
    setRecItem(row.item);
    setRecThreshold(String(row.auto_86_threshold));
    setRecLines(
      row.lines.map((line) => ({
        ingredient: line.ingredient,
        amount: milliToAmount(line.per_unit.milli),
      })),
    );
    setRecOpen(true);
  };

  const saveRecipe = async () => {
    const item = recItem().trim();
    if (!item) {
      setError(t("inventory.itemRequired"));
      return;
    }
    const threshold = Number(recThreshold().trim());
    if (!Number.isInteger(threshold) || threshold < 0) {
      setError(t("inventory.thresholdInvalid"));
      return;
    }
    const lines: RecipeLine[] = [];
    for (const draft of recLines()) {
      if (!draft.ingredient) {
        setError(t("inventory.lineIngredientRequired"));
        return;
      }
      const milli = amountToMilli(draft.amount);
      if (milli === null) {
        setError(t("inventory.lineAmountInvalid"));
        return;
      }
      lines.push({ ingredient: draft.ingredient, per_unit: { milli } });
    }
    const input: RecipeInput = { lines, auto_86_threshold: threshold };
    setBusy(true);
    try {
      const target = recEditing();
      if (target) {
        await api.updateRecipe(tenantId(), target.item, target.etag, input);
      } else {
        // A recipe is keyed by the item it makes, and that id comes from this form — so a create for
        // an item that already has one is refused rather than replacing its bill of materials
        // (ADR-0095). The editor reaches the update path instead.
        await api.createRecipe(tenantId(), item, input);
      }
      toast.ok(t("inventory.recipeSaved"));
      setRecOpen(false);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removeRecipe = async () => {
    const row = pendingRecDelete();
    if (!row) {
      return;
    }
    setBusy(true);
    try {
      await api.deleteRecipe(tenantId(), row.item);
      setPendingRecDelete(null);
      toast.ok(t("inventory.recipeDeleted"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- Suppliers ---

  const openSupCreate = () => {
    setSupEditing(null);
    setSupName("");
    setSupOpen(true);
  };

  const openSupEdit = (row: Supplier) => {
    setSupEditing({ id: row.id, etag: row.etag });
    setSupName(row.name);
    setSupOpen(true);
  };

  const saveSupplier = async () => {
    const name = supName().trim();
    if (!name) {
      setError(t("inventory.nameRequired"));
      return;
    }
    const input: SupplierInput = { name };
    setBusy(true);
    try {
      const target = supEditing();
      if (target) {
        await api.updateSupplier(tenantId(), target.id, target.etag, input);
        toast.ok(t("inventory.supplierSaved"));
      } else {
        await api.createSupplier(tenantId(), input);
        toast.ok(t("inventory.supplierSaved"));
      }
      setSupOpen(false);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removeSupplier = async () => {
    const row = pendingSupDelete();
    if (!row) {
      return;
    }
    setBusy(true);
    try {
      await api.deleteSupplier(tenantId(), row.id);
      setPendingSupDelete(null);
      toast.ok(t("inventory.supplierDeleted"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- Publish ---

  const publish = async () => {
    setBusy(true);
    try {
      const result = await api.publishInventory(tenantId(), storeId());
      toast.ok(t("inventory.published", { store: storeName(), version: result.config_version_id }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const ingredientColumns = (): Column<Ingredient>[] => [
    {
      key: "name",
      header: t("inventory.name"),
      sortValue: (row) => row.name,
      cell: (row) => <span class="font-medium text-ink">{row.name}</span>,
    },
    {
      key: "unit",
      header: t("inventory.unit"),
      cell: (row) => <StatusBadge tone="neutral" label={t(UNIT_LABEL[row.unit])} />,
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  const recipeColumns = (): Column<Recipe>[] => [
    {
      key: "item",
      header: t("inventory.item"),
      sortValue: (row) => itemName(row.item),
      cell: (row) => <span class="font-medium text-ink">{itemName(row.item)}</span>,
    },
    {
      key: "lines",
      header: t("inventory.lineCount"),
      sortValue: (row) => row.lines.length,
      cell: (row) => <span class="tabular-nums text-ink">{String(row.lines.length)}</span>,
    },
    {
      key: "threshold",
      header: t("inventory.threshold"),
      sortValue: (row) => row.auto_86_threshold,
      cell: (row) => (
        <span class="tabular-nums text-ink">{String(row.auto_86_threshold)}</span>
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.item}</TechnicalDetails>
      ),
    },
  ];

  const supplierColumns = (): Column<Supplier>[] => [
    {
      key: "name",
      header: t("inventory.name"),
      sortValue: (row) => row.name,
      cell: (row) => <span class="font-medium text-ink">{row.name}</span>,
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("inventory.title")} description={t("inventory.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          {/* Ingredients */}
          <Card
            title={t("inventory.ingredients")}
            actions={
              <div class="flex gap-2">
                <Button disabled={busy()} onClick={openIngCreate}>
                  {t("inventory.newIngredient")}
                </Button>
                <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <Show
              when={ingredients()}
              fallback={<p class="text-sm text-ink-muted">{t("inventory.loadHint")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={ingredientColumns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={10}
                  empty={
                    <EmptyState
                      title={t("inventory.ingredientsEmpty")}
                      description={t("inventory.ingredientsEmptyHint")}
                    />
                  }
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <div class="flex flex-wrap gap-2">
                      <Button variant="secondary" disabled={busy()} onClick={() => openIngEdit(row)}>
                        {t("action.edit")}
                      </Button>
                      <Button
                        variant="danger"
                        disabled={busy()}
                        onClick={() => setPendingIngDelete(row)}
                      >
                        {t("action.delete")}
                      </Button>
                    </div>
                  )}
                />
              )}
            </Show>
          </Card>

          {/* Recipes */}
          <Card
            title={t("inventory.recipes")}
            actions={
              <Button
                disabled={busy() || items().length === 0}
                onClick={openRecCreate}
              >
                {t("inventory.newRecipe")}
              </Button>
            }
          >
            <p class="mb-3 text-sm text-ink-muted">{t("inventory.recipesHint")}</p>
            <Show
              when={items().length > 0}
              fallback={<p class="text-sm text-ink-muted">{t("inventory.recipesNeedItems")}</p>}
            >
              <DataTable
                columns={recipeColumns()}
                rows={recipes()}
                searchText={(row) => itemName(row.item)}
                pageSize={10}
                empty={
                  <EmptyState
                    title={t("inventory.recipesEmpty")}
                    description={t("inventory.recipesEmptyHint")}
                  />
                }
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex flex-wrap gap-2">
                    <Button variant="secondary" disabled={busy()} onClick={() => openRecEdit(row)}>
                      {t("action.edit")}
                    </Button>
                    <Button
                      variant="danger"
                      disabled={busy()}
                      onClick={() => setPendingRecDelete(row)}
                    >
                      {t("action.delete")}
                    </Button>
                  </div>
                )}
              />
            </Show>
          </Card>

          {/* Suppliers */}
          <Card
            title={t("inventory.suppliers")}
            actions={
              <Button disabled={busy()} onClick={openSupCreate}>
                {t("inventory.newSupplier")}
              </Button>
            }
          >
            <DataTable
              columns={supplierColumns()}
              rows={suppliers()}
              searchText={(row) => row.name}
              pageSize={10}
              empty={
                <EmptyState
                  title={t("inventory.suppliersEmpty")}
                  description={t("inventory.suppliersEmptyHint")}
                />
              }
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <div class="flex flex-wrap gap-2">
                  <Button variant="secondary" disabled={busy()} onClick={() => openSupEdit(row)}>
                    {t("action.edit")}
                  </Button>
                  <Button
                    variant="danger"
                    disabled={busy()}
                    onClick={() => setPendingSupDelete(row)}
                  >
                    {t("action.delete")}
                  </Button>
                </div>
              )}
            />
          </Card>

          {/* Publish */}
          <Card title={t("inventory.publishTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("inventory.publishHint")}</p>
            <Show
              when={storeId()}
              fallback={<p class="text-sm text-ink-muted">{t("inventory.publishNeedsStore")}</p>}
            >
              <div class="flex flex-col gap-3">
                <p class="text-sm text-ink">{t("inventory.publishTo", { store: storeName() })}</p>
                <div>
                  <Button disabled={busy()} onClick={() => void publish()}>
                    {t("inventory.publish")}
                  </Button>
                </div>
              </div>
            </Show>
          </Card>
        </div>

        {/* Ingredient drawer */}
        <Drawer
          open={ingOpen()}
          title={ingEditing() ? t("inventory.editIngredient") : t("inventory.newIngredient")}
          closeLabel={t("action.close")}
          onClose={() => setIngOpen(false)}
          footer={
            <div class="flex gap-2">
              <Button disabled={busy()} onClick={() => void saveIngredient()}>
                {t("action.save")}
              </Button>
              <Button variant="secondary" disabled={busy()} onClick={() => setIngOpen(false)}>
                {t("action.cancel")}
              </Button>
            </div>
          }
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("inventory.name")}
              value={ingName()}
              onInput={setIngName}
              placeholder={t("inventory.ingredientNamePlaceholder")}
            />
            <label class="block">
              <span class="mb-1 block text-sm font-medium text-ink">{t("inventory.unit")}</span>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                value={ingUnit()}
                onChange={(event) => setIngUnit(event.currentTarget.value as UnitOfMeasure)}
              >
                <For each={UNITS}>
                  {(unit) => <option value={unit}>{t(UNIT_LABEL[unit])}</option>}
                </For>
              </select>
            </label>
          </div>
        </Drawer>

        {/* Recipe drawer */}
        <Drawer
          open={recOpen()}
          title={recEditing() ? t("inventory.editRecipe") : t("inventory.newRecipe")}
          closeLabel={t("action.close")}
          onClose={() => setRecOpen(false)}
          footer={
            <div class="flex gap-2">
              <Button disabled={busy()} onClick={() => void saveRecipe()}>
                {t("action.save")}
              </Button>
              <Button variant="secondary" disabled={busy()} onClick={() => setRecOpen(false)}>
                {t("action.cancel")}
              </Button>
            </div>
          }
        >
          <div class="flex flex-col gap-4">
            <label class="block">
              <span class="mb-1 block text-sm font-medium text-ink">{t("inventory.item")}</span>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-60"
                value={recItem()}
                disabled={recEditing() !== null}
                onChange={(event) => setRecItem(event.currentTarget.value)}
              >
                <For each={items()}>
                  {(item) => <option value={item.menu_item_id}>{item.name}</option>}
                </For>
              </select>
              <Show when={recEditing() !== null}>
                <span class="mt-1 block text-sm text-ink-muted">{t("inventory.itemFixed")}</span>
              </Show>
            </label>

            <TextField
              label={t("inventory.threshold")}
              type="number"
              value={recThreshold()}
              onInput={setRecThreshold}
            />
            <p class="text-sm text-ink-muted">{t("inventory.thresholdHint")}</p>

            <fieldset class="border-t border-line pt-4">
              <legend class="mb-1 text-sm font-medium text-ink">{t("inventory.bom")}</legend>
              <p class="mb-2 text-sm text-ink-muted">{t("inventory.bomHint")}</p>
              <Show
                when={(ingredients() ?? []).length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("inventory.bomNeedsIngredients")}</p>}
              >
                <div class="flex flex-col gap-2">
                  <For each={recLines()}>
                    {(line, index) => (
                      <div class="flex flex-wrap items-end gap-2">
                        <label class="block grow">
                          <span class="mb-1 block text-sm font-medium text-ink">
                            {t("inventory.lineIngredient")}
                          </span>
                          <select
                            class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                            value={line.ingredient}
                            onChange={(event) =>
                              setRecLines((prev) =>
                                prev.map((l, i) =>
                                  i === index()
                                    ? { ...l, ingredient: event.currentTarget.value }
                                    : l,
                                ),
                              )
                            }
                          >
                            <option value="">{t("inventory.lineIngredientPick")}</option>
                            <For each={ingredients() ?? []}>
                              {(ing) => (
                                <option value={ing.id}>
                                  {ing.name} ({t(UNIT_LABEL[ing.unit])})
                                </option>
                              )}
                            </For>
                          </select>
                        </label>
                        <label class="block w-28">
                          <span class="mb-1 block text-sm font-medium text-ink">
                            {t("inventory.linePerUnit")}
                          </span>
                          <input
                            type="number"
                            step="0.001"
                            class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                            aria-label={t("inventory.linePerUnit")}
                            value={line.amount}
                            onInput={(event) =>
                              setRecLines((prev) =>
                                prev.map((l, i) =>
                                  i === index() ? { ...l, amount: event.currentTarget.value } : l,
                                ),
                              )
                            }
                          />
                        </label>
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() =>
                            setRecLines((prev) => prev.filter((_, i) => i !== index()))
                          }
                        >
                          {t("inventory.lineRemove")}
                        </Button>
                      </div>
                    )}
                  </For>
                  <div>
                    <Button
                      variant="secondary"
                      disabled={busy()}
                      onClick={() => setRecLines((prev) => [...prev, { ingredient: "", amount: "" }])}
                    >
                      {t("inventory.lineAdd")}
                    </Button>
                  </div>
                </div>
              </Show>
            </fieldset>
          </div>
        </Drawer>

        {/* Supplier drawer */}
        <Drawer
          open={supOpen()}
          title={supEditing() ? t("inventory.editSupplier") : t("inventory.newSupplier")}
          closeLabel={t("action.close")}
          onClose={() => setSupOpen(false)}
          footer={
            <div class="flex gap-2">
              <Button disabled={busy()} onClick={() => void saveSupplier()}>
                {t("action.save")}
              </Button>
              <Button variant="secondary" disabled={busy()} onClick={() => setSupOpen(false)}>
                {t("action.cancel")}
              </Button>
            </div>
          }
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("inventory.name")}
              value={supName()}
              onInput={setSupName}
              placeholder={t("inventory.supplierNamePlaceholder")}
            />
            <p class="text-sm text-ink-muted">{t("inventory.supplierHint")}</p>
          </div>
        </Drawer>

        <ConfirmDialog
          open={pendingIngDelete() !== null}
          title={t("inventory.deleteIngredientTitle")}
          message={t("inventory.deleteIngredientMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void removeIngredient()}
          onCancel={() => setPendingIngDelete(null)}
        />

        <ConfirmDialog
          open={pendingRecDelete() !== null}
          title={t("inventory.deleteRecipeTitle")}
          message={t("inventory.deleteRecipeMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void removeRecipe()}
          onCancel={() => setPendingRecDelete(null)}
        />

        <ConfirmDialog
          open={pendingSupDelete() !== null}
          title={t("inventory.deleteSupplierTitle")}
          message={t("inventory.deleteSupplierMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void removeSupplier()}
          onCancel={() => setPendingSupDelete(null)}
        />
      </RequireContext>
    </div>
  );
}
