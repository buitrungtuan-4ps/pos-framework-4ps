// The layout editor (ADR-0066, Phase 2a, entities 11/12). The presentation half of the catalog, kept
// deliberately apart from the price half (the Menu screen): a screen's display taxonomy and the
// per-channel buttons it shows. Publishing (on the Menu screen) compiles these into the `layout`
// config node alongside the priced `menu` node — a button moving reprices nothing. Everything is by
// name; tenant comes from the top-bar context. `pos-core` never reads any of this — only the POS /
// tablet / QR UI does.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type {
  CatalogItem,
  DisplayCategory,
  DisplaySubcategory,
  EntityStatus,
  LayoutButton,
  SalesChannel,
} from "../api/types";
import { SALES_CHANNELS } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

const CHANNEL_LABEL: Record<SalesChannel, MessageKey> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

export function Layout() {
  const [items, setItems] = createSignal<CatalogItem[]>([]);
  const [categories, setCategories] = createSignal<DisplayCategory[]>([]);
  const [subcategories, setSubcategories] = createSignal<DisplaySubcategory[]>([]);
  const [buttons, setButtons] = createSignal<LayoutButton[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [channel, setChannel] = createSignal<SalesChannel>("SALES_CHANNEL_DINE_IN");

  const [newCategoryName, setNewCategoryName] = createSignal("");
  const [newSubcategoryName, setNewSubcategoryName] = createSignal("");
  const [newSubcategoryParent, setNewSubcategoryParent] = createSignal("");
  const [editingCategory, setEditingCategory] = createSignal("");
  const [editingSubcategory, setEditingSubcategory] = createSignal("");
  const [draftName, setDraftName] = createSignal("");

  // Button editor.
  const [buttonItem, setButtonItem] = createSignal("");
  const [buttonCategory, setButtonCategory] = createSignal("");
  const [buttonSubcategory, setButtonSubcategory] = createSignal("");
  const [buttonLabel, setButtonLabel] = createSignal("");
  const [buttonColumn, setButtonColumn] = createSignal("");
  const [buttonRow, setButtonRow] = createSignal("");
  const [buttonSort, setButtonSort] = createSignal("0");
  const [buttonEditing, setButtonEditing] = createSignal(false);

  const fail = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : String(caught));

  const itemName = (id: string) =>
    items().find((item) => item.menu_item_id === id)?.name ?? id;
  const categoryName = (id: string) =>
    categories().find((row) => row.display_category_id === id)?.name ?? id;
  const subcategoryName = (id: string | null) =>
    id ? (subcategories().find((row) => row.display_subcategory_id === id)?.name ?? id) : "—";

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedItems, loadedCategories, loadedSubcategories, loadedButtons] = await Promise.all(
        [
          api.listItems(tenantId()),
          api.listDisplayCategories(tenantId()),
          api.listDisplaySubcategories(tenantId()),
          api.listLayoutButtons(tenantId()),
        ],
      );
      setItems(loadedItems);
      setCategories(loadedCategories);
      setSubcategories(loadedSubcategories);
      setButtons(loadedButtons);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const reloadButtons = async () => {
    setButtons(await api.listLayoutButtons(tenantId()));
  };

  // --- display taxonomy ---

  const createCategory = async () => {
    const name = newCategoryName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createDisplayCategory(tenantId(), name);
      setNewCategoryName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setCategoryFields = async (
    row: DisplayCategory,
    fields: { name?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateDisplayCategory(row.display_category_id, tenantId(), {
        name,
        status: fields.status ?? row.status,
      });
      setEditingCategory("");
      setDraftName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const createSubcategory = async () => {
    const name = newSubcategoryName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    if (!newSubcategoryParent()) {
      setError(t("layout.parentCategoryRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createDisplaySubcategory(tenantId(), newSubcategoryParent(), name);
      setNewSubcategoryName("");
      setNewSubcategoryParent("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setSubcategoryFields = async (
    row: DisplaySubcategory,
    fields: { name?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateDisplaySubcategory(row.display_subcategory_id, tenantId(), {
        displayCategoryId: row.display_category_id,
        name,
        status: fields.status ?? row.status,
      });
      setEditingSubcategory("");
      setDraftName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- layout buttons ---

  const channelButtons = () =>
    (buttons() ?? [])
      .filter((button) => button.sales_channel === channel())
      .slice()
      .sort((a, b) => a.sort - b.sort);

  const resetButtonEditor = () => {
    setButtonItem("");
    setButtonCategory("");
    setButtonSubcategory("");
    setButtonLabel("");
    setButtonColumn("");
    setButtonRow("");
    setButtonSort("0");
    setButtonEditing(false);
  };

  const editButton = (button: LayoutButton) => {
    setButtonItem(button.menu_item_id);
    setButtonCategory(button.display_category_id);
    setButtonSubcategory(button.display_subcategory_id ?? "");
    setButtonLabel(button.label);
    setButtonColumn(button.position ? String(button.position.column) : "");
    setButtonRow(button.position ? String(button.position.row) : "");
    setButtonSort(String(button.sort));
    setButtonEditing(true);
  };

  const saveButton = async () => {
    if (!buttonItem()) {
      setError(t("layout.itemRequired"));
      return;
    }
    if (!buttonCategory()) {
      setError(t("layout.categoryRequired"));
      return;
    }
    const label = buttonLabel().trim();
    if (!label) {
      setError(t("layout.labelRequired"));
      return;
    }
    // A grid slot needs both column and row, or neither (a flowing button).
    const rawColumn = buttonColumn().trim();
    const rawRow = buttonRow().trim();
    let gridColumn: number | null = null;
    let gridRow: number | null = null;
    if (rawColumn || rawRow) {
      const column = Number(rawColumn);
      const row = Number(rawRow);
      if (!Number.isInteger(column) || column < 0 || !Number.isInteger(row) || row < 0) {
        setError(t("layout.gridInvalid"));
        return;
      }
      gridColumn = column;
      gridRow = row;
    }
    const sort = Number(buttonSort().trim() || "0");
    if (!Number.isInteger(sort)) {
      setError(t("layout.sortInvalid"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.setLayoutButton(tenantId(), channel(), buttonItem(), {
        displayCategoryId: buttonCategory(),
        displaySubcategoryId: buttonSubcategory() || null,
        label,
        gridColumn,
        gridRow,
        sort,
      });
      resetButtonEditor();
      await reloadButtons();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removeButton = async (menuItemId: string) => {
    setError("");
    setBusy(true);
    try {
      await api.removeLayoutButton(tenantId(), channel(), menuItemId);
      await reloadButtons();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const gridLabel = (button: LayoutButton) =>
    button.position ? `${button.position.column},${button.position.row}` : t("layout.flowing");

  return (
    <div>
      <PageHeader title={t("layout.title")} description={t("layout.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Card
            title={t("layout.buttons")}
            actions={
              <div class="flex flex-wrap items-center gap-2">
                <label class="text-sm text-ink-muted">
                  <span class="sr-only">{t("layout.channel")}</span>
                  <select
                    class="min-h-touch rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                    value={channel()}
                    onChange={(event) => {
                      setChannel(event.currentTarget.value as SalesChannel);
                      resetButtonEditor();
                    }}
                  >
                    <For each={SALES_CHANNELS}>
                      {(row) => <option value={row}>{t(CHANNEL_LABEL[row])}</option>}
                    </For>
                  </select>
                </label>
                <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <div class="flex flex-col gap-4">
              <Show
                when={buttons()}
                fallback={<p class="text-sm text-ink-muted">{t("layout.loadHint")}</p>}
              >
                <Show
                  when={channelButtons().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("layout.buttonsEmpty")}</p>}
                >
                  <div class="overflow-x-auto">
                    <table class="w-full text-left text-sm">
                      <thead>
                        <tr class="border-b border-line text-ink-muted">
                          <th class="py-2 pr-4 font-medium">{t("layout.sort")}</th>
                          <th class="py-2 pr-4 font-medium">{t("layout.item")}</th>
                          <th class="py-2 pr-4 font-medium">{t("layout.label")}</th>
                          <th class="py-2 pr-4 font-medium">{t("layout.category")}</th>
                          <th class="py-2 pr-4 font-medium">{t("layout.subcategory")}</th>
                          <th class="py-2 pr-4 font-medium">{t("layout.grid")}</th>
                          <th class="py-2 font-medium">{t("catalog.actions")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={channelButtons()}>
                          {(button) => (
                            <tr class="border-b border-line align-top text-ink">
                              <td class="py-2 pr-4">{button.sort}</td>
                              <td class="py-2 pr-4">{itemName(button.menu_item_id)}</td>
                              <td class="py-2 pr-4">{button.label}</td>
                              <td class="py-2 pr-4">{categoryName(button.display_category_id)}</td>
                              <td class="py-2 pr-4">
                                {subcategoryName(button.display_subcategory_id)}
                              </td>
                              <td class="py-2 pr-4">{gridLabel(button)}</td>
                              <td class="flex flex-wrap gap-2 py-2">
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => editButton(button)}
                                >
                                  {t("catalog.edit")}
                                </Button>
                                <Button
                                  variant="danger"
                                  disabled={busy()}
                                  onClick={() => void removeButton(button.menu_item_id)}
                                >
                                  {t("catalog.remove")}
                                </Button>
                              </td>
                            </tr>
                          )}
                        </For>
                      </tbody>
                    </table>
                  </div>
                </Show>
              </Show>

              <div class="rounded-token border border-line bg-surface-raised p-4">
                <h3 class="mb-3 text-base font-semibold text-ink">
                  {buttonEditing() ? t("layout.editButton") : t("layout.addButton")}
                </h3>
                <div class="grid gap-4 md:grid-cols-2">
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">{t("layout.item")}</span>
                    <select
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink disabled:opacity-50"
                      value={buttonItem()}
                      disabled={buttonEditing()}
                      onChange={(event) => setButtonItem(event.currentTarget.value)}
                    >
                      <option value="">{t("layout.chooseItem")}</option>
                      <For each={items().filter((item) => item.status === "active")}>
                        {(item) => <option value={item.menu_item_id}>{item.name}</option>}
                      </For>
                    </select>
                  </label>
                  <TextField
                    label={t("layout.label")}
                    value={buttonLabel()}
                    onInput={setButtonLabel}
                    placeholder={t("layout.labelPlaceholder")}
                  />
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("layout.category")}
                    </span>
                    <select
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                      value={buttonCategory()}
                      onChange={(event) => {
                        setButtonCategory(event.currentTarget.value);
                        setButtonSubcategory("");
                      }}
                    >
                      <option value="">{t("layout.chooseCategory")}</option>
                      <For each={categories().filter((row) => row.status === "active")}>
                        {(row) => <option value={row.display_category_id}>{row.name}</option>}
                      </For>
                    </select>
                  </label>
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("layout.subcategory")}
                    </span>
                    <select
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink disabled:opacity-50"
                      value={buttonSubcategory()}
                      disabled={!buttonCategory()}
                      onChange={(event) => setButtonSubcategory(event.currentTarget.value)}
                    >
                      <option value="">{t("layout.noSubcategory")}</option>
                      <For
                        each={subcategories().filter(
                          (row) =>
                            row.status === "active" &&
                            row.display_category_id === buttonCategory(),
                        )}
                      >
                        {(row) => <option value={row.display_subcategory_id}>{row.name}</option>}
                      </For>
                    </select>
                  </label>
                </div>
                <p class="mt-3 mb-2 text-xs text-ink-muted">{t("layout.gridHint")}</p>
                <div class="grid gap-3 md:grid-cols-3">
                  <label class="block">
                    <span class="mb-1 block text-sm text-ink">{t("layout.gridColumn")}</span>
                    <input
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                      inputmode="numeric"
                      aria-label={t("layout.gridColumn")}
                      value={buttonColumn()}
                      onInput={(event) => setButtonColumn(event.currentTarget.value)}
                    />
                  </label>
                  <label class="block">
                    <span class="mb-1 block text-sm text-ink">{t("layout.gridRow")}</span>
                    <input
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                      inputmode="numeric"
                      aria-label={t("layout.gridRow")}
                      value={buttonRow()}
                      onInput={(event) => setButtonRow(event.currentTarget.value)}
                    />
                  </label>
                  <label class="block">
                    <span class="mb-1 block text-sm text-ink">{t("layout.sort")}</span>
                    <input
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                      inputmode="numeric"
                      aria-label={t("layout.sort")}
                      value={buttonSort()}
                      onInput={(event) => setButtonSort(event.currentTarget.value)}
                    />
                  </label>
                </div>
                <div class="mt-4 flex flex-wrap gap-2">
                  <Button disabled={busy()} onClick={() => void saveButton()}>
                    {t("layout.saveButton")}
                  </Button>
                  <Show when={buttonEditing()}>
                    <Button variant="secondary" onClick={resetButtonEditor}>
                      {t("action.cancel")}
                    </Button>
                  </Show>
                </div>
              </div>
            </div>
          </Card>

          <div class="grid gap-6 lg:grid-cols-2">
            <Card title={t("layout.categories")}>
              <div class="flex flex-col gap-4">
                <Show
                  when={categories().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("layout.categoriesEmpty")}</p>}
                >
                  <ul class="flex flex-col gap-2">
                    <For each={categories()}>
                      {(row) => (
                        <li class="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2 text-sm text-ink">
                          <Show
                            when={editingCategory() === row.display_category_id}
                            fallback={
                              <span>
                                {row.name}
                                <Show when={row.status === "archived"}>
                                  <span class="ml-2 text-xs text-ink-muted">
                                    ({t("catalog.statusArchived")})
                                  </span>
                                </Show>
                              </span>
                            }
                          >
                            <input
                              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                              aria-label={t("layout.categoryName")}
                              value={draftName()}
                              onInput={(event) => setDraftName(event.currentTarget.value)}
                            />
                          </Show>
                          <div class="flex flex-wrap gap-2">
                            <Show
                              when={editingCategory() === row.display_category_id}
                              fallback={
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingCategory(row.display_category_id);
                                    setDraftName(row.name);
                                  }}
                                >
                                  {t("catalog.rename")}
                                </Button>
                              }
                            >
                              <Button
                                disabled={busy()}
                                onClick={() => void setCategoryFields(row, { name: draftName() })}
                              >
                                {t("action.save")}
                              </Button>
                              <Button
                                variant="secondary"
                                onClick={() => {
                                  setEditingCategory("");
                                  setDraftName("");
                                }}
                              >
                                {t("action.cancel")}
                              </Button>
                            </Show>
                            <Button
                              variant={row.status === "archived" ? "secondary" : "danger"}
                              disabled={busy()}
                              onClick={() =>
                                void setCategoryFields(row, {
                                  status: row.status === "archived" ? "active" : "archived",
                                })
                              }
                            >
                              {row.status === "archived"
                                ? t("catalog.restore")
                                : t("catalog.archive")}
                            </Button>
                          </div>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
                <div class="grid gap-4 md:grid-cols-2 md:items-end">
                  <TextField
                    label={t("layout.categoryName")}
                    value={newCategoryName()}
                    onInput={setNewCategoryName}
                    placeholder={t("layout.categoryNamePlaceholder")}
                  />
                  <Button disabled={busy()} onClick={() => void createCategory()}>
                    {t("action.create")}
                  </Button>
                </div>
              </div>
            </Card>

            <Card title={t("layout.subcategories")}>
              <div class="flex flex-col gap-4">
                <Show
                  when={subcategories().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("layout.subcategoriesEmpty")}</p>}
                >
                  <ul class="flex flex-col gap-2">
                    <For each={subcategories()}>
                      {(row) => (
                        <li class="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2 text-sm text-ink">
                          <Show
                            when={editingSubcategory() === row.display_subcategory_id}
                            fallback={
                              <span>
                                {row.name}
                                <span class="ml-2 text-xs text-ink-muted">
                                  {categoryName(row.display_category_id)}
                                </span>
                              </span>
                            }
                          >
                            <input
                              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                              aria-label={t("layout.subcategoryName")}
                              value={draftName()}
                              onInput={(event) => setDraftName(event.currentTarget.value)}
                            />
                          </Show>
                          <div class="flex flex-wrap gap-2">
                            <Show
                              when={editingSubcategory() === row.display_subcategory_id}
                              fallback={
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingSubcategory(row.display_subcategory_id);
                                    setDraftName(row.name);
                                  }}
                                >
                                  {t("catalog.rename")}
                                </Button>
                              }
                            >
                              <Button
                                disabled={busy()}
                                onClick={() => void setSubcategoryFields(row, { name: draftName() })}
                              >
                                {t("action.save")}
                              </Button>
                              <Button
                                variant="secondary"
                                onClick={() => {
                                  setEditingSubcategory("");
                                  setDraftName("");
                                }}
                              >
                                {t("action.cancel")}
                              </Button>
                            </Show>
                            <Button
                              variant={row.status === "archived" ? "secondary" : "danger"}
                              disabled={busy()}
                              onClick={() =>
                                void setSubcategoryFields(row, {
                                  status: row.status === "archived" ? "active" : "archived",
                                })
                              }
                            >
                              {row.status === "archived"
                                ? t("catalog.restore")
                                : t("catalog.archive")}
                            </Button>
                          </div>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
                <div class="grid gap-4 md:items-end">
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("layout.parentCategory")}
                    </span>
                    <select
                      class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                      value={newSubcategoryParent()}
                      onChange={(event) => setNewSubcategoryParent(event.currentTarget.value)}
                    >
                      <option value="">{t("layout.chooseCategory")}</option>
                      <For each={categories().filter((row) => row.status === "active")}>
                        {(row) => <option value={row.display_category_id}>{row.name}</option>}
                      </For>
                    </select>
                  </label>
                  <TextField
                    label={t("layout.subcategoryName")}
                    value={newSubcategoryName()}
                    onInput={setNewSubcategoryName}
                    placeholder={t("layout.subcategoryNamePlaceholder")}
                  />
                  <Button disabled={busy()} onClick={() => void createSubcategory()}>
                    {t("action.create")}
                  </Button>
                </div>
              </div>
            </Card>
          </div>
        </div>
      </RequireContext>
    </div>
  );
}
