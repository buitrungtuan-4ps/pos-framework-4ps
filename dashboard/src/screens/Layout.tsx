// The layout editor (ADR-0066 entities 11/12; rebuilt on the F2 kit for Track F3, ADR-0082). The
// presentation half of the catalog, kept apart from the price half (the Menu screen): a screen's
// display taxonomy and the per-channel buttons it shows. Publishing (on the Menu screen) compiles
// these into the `layout` config node alongside the priced `menu` node — moving a button reprices
// nothing. `pos-core` never reads any of this; only the POS / tablet / QR UI does.
//
// F3 turns the hand-typed integer grid into a visual editor: per channel, positioned buttons render
// on a device-shaped grid preview (with a client-side collision check on shared cells), flowing
// buttons reorder through the kit's `ReorderList`, and a channel's buttons can be copied to another
// channel. The display taxonomy moves onto kit `DataTable`s + `Drawer`s like the rest of F3.

import { createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import type {
  CatalogItem,
  DisplayCategory,
  DisplaySubcategory,
  ETag,
  LayoutButton,
  SalesChannel,
} from "../api/types";
import { SALES_CHANNELS } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  FormField,
  ReorderList,
} from "../components/kit";
import { toast } from "../components/Toast";
import { CHANNEL_LABEL, errorMessage, isStale, StatusCell } from "./catalog/shared";

// Cap the rendered grid extent so a stray large column/row never blows up the preview; buttons beyond
// it still appear in the flowing list and are editable.
const MAX_GRID = 20;

export function Layout() {
  const [items, setItems] = createSignal<CatalogItem[]>([]);
  const [categories, setCategories] = createSignal<DisplayCategory[] | null>(null);
  const [subcategories, setSubcategories] = createSignal<DisplaySubcategory[] | null>(null);
  const [buttons, setButtons] = createSignal<LayoutButton[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [channel, setChannel] = createSignal<SalesChannel>("SALES_CHANNEL_DINE_IN");

  // Button editor drawer.
  const [buttonOpen, setButtonOpen] = createSignal(false);
  // Non-null while editing an existing button, carrying the version it was read at: the update is
  // conditional on that version (ADR-0095). `null` means the drawer is placing a new one.
  const [buttonEditing, setButtonEditing] = createSignal<{ etag: ETag } | null>(null);
  const [buttonItem, setButtonItem] = createSignal("");
  const [buttonCategory, setButtonCategory] = createSignal("");
  const [buttonSubcategory, setButtonSubcategory] = createSignal("");
  const [buttonLabel, setButtonLabel] = createSignal("");
  const [buttonColumn, setButtonColumn] = createSignal("");
  const [buttonRow, setButtonRow] = createSignal("");
  const [buttonSort, setButtonSort] = createSignal(0);
  const [pendingRemove, setPendingRemove] = createSignal<LayoutButton | null>(null);

  // Copy-between-channels.
  const [copyTarget, setCopyTarget] = createSignal<SalesChannel | "">("");
  const [copyConfirm, setCopyConfirm] = createSignal(false);

  // Display category create/edit.
  const [creatingCategory, setCreatingCategory] = createSignal(false);
  const [newCategoryName, setNewCategoryName] = createSignal("");
  const [editingCategory, setEditingCategory] = createSignal<DisplayCategory | null>(null);

  // Display sub-category create/edit.
  const [creatingSubcategory, setCreatingSubcategory] = createSignal(false);
  const [newSubcategoryName, setNewSubcategoryName] = createSignal("");
  const [newSubcategoryParent, setNewSubcategoryParent] = createSignal("");
  const [editingSubcategory, setEditingSubcategory] = createSignal<DisplaySubcategory | null>(null);

  // Shared draft name for whichever taxonomy edit drawer is open.
  const [draftName, setDraftName] = createSignal("");

  const itemName = (id: string) => items().find((item) => item.menu_item_id === id)?.name ?? id;
  const categoryName = (id: string) =>
    (categories() ?? []).find((row) => row.display_category_id === id)?.name ?? id;
  const activeCategories = () => (categories() ?? []).filter((row) => row.status === "active");
  const activeSubcategories = (categoryId: string) =>
    (subcategories() ?? []).filter(
      (row) => row.status === "active" && row.display_category_id === categoryId,
    );

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedItems, loadedCategories, loadedSubcategories, loadedButtons] = await Promise.all([
        api.listItems(tenantId()),
        api.listDisplayCategories(tenantId()),
        api.listDisplaySubcategories(tenantId()),
        api.listLayoutButtons(tenantId()),
      ]);
      setItems(loadedItems);
      setCategories(loadedCategories);
      setSubcategories(loadedSubcategories);
      setButtons(loadedButtons);
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const reloadButtons = async () => {
    setButtons(await api.listLayoutButtons(tenantId()));
  };

  // --- per-channel button views ---

  const channelButtons = () =>
    (buttons() ?? [])
      .filter((button) => button.sales_channel === channel())
      .slice()
      .sort((a, b) => a.sort - b.sort);
  const positionedButtons = () => channelButtons().filter((button) => button.position);
  const flowingButtons = () => channelButtons().filter((button) => !button.position);

  const cellKey = (column: number, row: number) => `${column},${row}`;

  // Cells holding more than one positioned button — the POS shows only one, so flag them.
  const collisionCells = () => {
    const counts = new Map<string, number>();
    for (const button of positionedButtons()) {
      if (button.position) {
        const key = cellKey(button.position.column, button.position.row);
        counts.set(key, (counts.get(key) ?? 0) + 1);
      }
    }
    return new Set([...counts.entries()].filter(([, count]) => count > 1).map(([key]) => key));
  };

  const gridExtent = () => {
    let cols = 1;
    let rows = 1;
    for (const button of positionedButtons()) {
      if (button.position) {
        cols = Math.max(cols, button.position.column + 1);
        rows = Math.max(rows, button.position.row + 1);
      }
    }
    return { cols: Math.min(MAX_GRID, cols), rows: Math.min(MAX_GRID, rows) };
  };

  // A reactive rows×cols grid of the buttons at each cell — usually one, more than one on a collision.
  // Derived (not index-mapped) so it recomputes whenever the buttons change, and so a colliding button
  // is shown stacked rather than hidden behind the cell's first occupant.
  const grid = (): LayoutButton[][][] => {
    const { cols, rows } = gridExtent();
    const positioned = positionedButtons();
    const out: LayoutButton[][][] = [];
    for (let row = 0; row < rows; row += 1) {
      const cells: LayoutButton[][] = [];
      for (let column = 0; column < cols; column += 1) {
        cells.push(
          positioned.filter(
            (button) => button.position?.column === column && button.position?.row === row,
          ),
        );
      }
      out.push(cells);
    }
    return out;
  };

  // --- button editor ---

  const openAddButton = () => {
    setButtonEditing(null);
    setButtonItem("");
    setButtonCategory("");
    setButtonSubcategory("");
    setButtonLabel("");
    setButtonColumn("");
    setButtonRow("");
    setButtonSort(channelButtons().length);
    setButtonOpen(true);
  };

  const openEditButton = (button: LayoutButton) => {
    setButtonEditing({ etag: button.etag });
    setButtonItem(button.menu_item_id);
    setButtonCategory(button.display_category_id);
    setButtonSubcategory(button.display_subcategory_id ?? "");
    setButtonLabel(button.label);
    setButtonColumn(button.position ? String(button.position.column) : "");
    setButtonRow(button.position ? String(button.position.row) : "");
    setButtonSort(button.sort);
    setButtonOpen(true);
  };

  const saveButton = async () => {
    if (!buttonItem()) {
      toast.error(t("layout.itemRequired"));
      return;
    }
    if (!buttonCategory()) {
      toast.error(t("layout.categoryRequired"));
      return;
    }
    const label = buttonLabel().trim();
    if (!label) {
      toast.error(t("layout.labelRequired"));
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
        toast.error(t("layout.gridInvalid"));
        return;
      }
      gridColumn = column;
      gridRow = row;
    }
    const fields = {
      displayCategoryId: buttonCategory(),
      displaySubcategoryId: buttonSubcategory() || null,
      label,
      gridColumn,
      gridRow,
      sort: buttonSort(),
    };
    setBusy(true);
    try {
      const editing = buttonEditing();
      if (editing) {
        await api.updateLayoutButton(tenantId(), channel(), buttonItem(), editing.etag, fields);
      } else {
        // A button's slot is its `(channel, item)` pair, supplied here — so placing one where a
        // button already sits is refused rather than relabelling and re-positioning it (ADR-0095).
        await api.createLayoutButton(tenantId(), channel(), buttonItem(), fields);
      }
      toast.ok(t("layout.buttonSaved"));
      setButtonOpen(false);
      await reloadButtons();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const removeButton = async () => {
    const button = pendingRemove();
    if (!button) {
      return;
    }
    setBusy(true);
    try {
      await api.removeLayoutButton(tenantId(), channel(), button.menu_item_id);
      toast.ok(t("layout.buttonRemoved"));
      setPendingRemove(null);
      setButtonOpen(false);
      await reloadButtons();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Reorder the flowing buttons: rewrite each one's sort to its new index (positioned buttons keep
  // theirs; grid position, not sort, places them).
  const reorderFlowing = async (from: number, to: number) => {
    const order = flowingButtons();
    if (to < 0 || to >= order.length || from === to) {
      return;
    }
    const next = order.slice();
    const moved = next.splice(from, 1)[0];
    if (!moved) {
      return;
    }
    next.splice(to, 0, moved);
    setBusy(true);
    try {
      for (const [index, button] of next.entries()) {
        if (button.sort !== index) {
          // Every button here was just read, so each write is conditional on the version it carried:
          // a reorder against a layout someone else has edited is refused, not merged blindly.
          await api.updateLayoutButton(
            tenantId(),
            channel(),
            button.menu_item_id,
            button.etag,
            {
              displayCategoryId: button.display_category_id,
              displaySubcategoryId: button.display_subcategory_id,
              label: button.label,
              gridColumn: null,
              gridRow: null,
              sort: index,
            },
          );
        }
      }
      await reloadButtons();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // --- copy between channels ---

  const doCopy = async () => {
    const target = copyTarget();
    if (!target) {
      return;
    }
    const source = channelButtons();
    // What the target channel already has, by item. A copy still overwrites — that is what the
    // confirmation promises — but each overwrite now goes through the update at the version the row
    // was read at, so a button someone edited since this page loaded refuses the write instead of
    // losing their change (ADR-0095).
    const onTarget = new Map(
      (buttons() ?? [])
        .filter((button) => button.sales_channel === target)
        .map((button) => [button.menu_item_id, button] as const),
    );
    setBusy(true);
    try {
      for (const button of source) {
        const fields = {
          displayCategoryId: button.display_category_id,
          displaySubcategoryId: button.display_subcategory_id,
          label: button.label,
          gridColumn: button.position?.column ?? null,
          gridRow: button.position?.row ?? null,
          sort: button.sort,
        };
        const already = onTarget.get(button.menu_item_id);
        if (already) {
          await api.updateLayoutButton(
            tenantId(),
            target,
            button.menu_item_id,
            already.etag,
            fields,
          );
        } else {
          await api.createLayoutButton(tenantId(), target, button.menu_item_id, fields);
        }
      }
      toast.ok(t("layout.copied", { count: source.length, channel: t(CHANNEL_LABEL[target]) }));
      setCopyConfirm(false);
      setCopyTarget("");
      await reloadButtons();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // --- display taxonomy ---

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
      await api.createDisplayCategory(tenantId(), name);
      toast.ok(t("layout.categoryCreated"));
      setCreatingCategory(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applyCategory = async (
    row: DisplayCategory,
    fields: { name?: string; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateDisplayCategory(row.display_category_id, tenantId(), {
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

  const openEditCategory = (row: DisplayCategory) => {
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
      toast.ok(t("layout.categorySaved"));
      setEditingCategory(null);
    }
  };

  const toggleCategory = async (row: DisplayCategory) => {
    const archiving = row.status !== "archived";
    const ok = await applyCategory(row, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("layout.categoryArchived") : t("layout.categoryRestored"));
    }
  };

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
      toast.error(t("layout.parentCategoryRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createDisplaySubcategory(tenantId(), newSubcategoryParent(), name);
      toast.ok(t("layout.subcategoryCreated"));
      setCreatingSubcategory(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applySubcategory = async (
    row: DisplaySubcategory,
    fields: { name?: string; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? row.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateDisplaySubcategory(row.display_subcategory_id, tenantId(), {
        displayCategoryId: row.display_category_id,
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

  const openEditSubcategory = (row: DisplaySubcategory) => {
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
      toast.ok(t("layout.subcategorySaved"));
      setEditingSubcategory(null);
    }
  };

  const toggleSubcategory = async (row: DisplaySubcategory) => {
    const archiving = row.status !== "archived";
    const ok = await applySubcategory(row, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("layout.subcategoryArchived") : t("layout.subcategoryRestored"));
    }
  };

  const categoryColumns = (): Column<DisplayCategory>[] => [
    { key: "name", header: t("catalog.name"), sortValue: (row) => row.name, cell: (row) => row.name },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  const subcategoryColumns = (): Column<DisplaySubcategory>[] => [
    { key: "name", header: t("catalog.name"), sortValue: (row) => row.name, cell: (row) => row.name },
    {
      key: "parent",
      header: t("layout.parentCategory"),
      sortValue: (row) => categoryName(row.display_category_id),
      cell: (row) => categoryName(row.display_category_id),
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

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
                    onChange={(event) => setChannel(event.currentTarget.value as SalesChannel)}
                  >
                    <For each={SALES_CHANNELS}>
                      {(row) => <option value={row}>{t(CHANNEL_LABEL[row])}</option>}
                    </For>
                  </select>
                </label>
                <Button disabled={busy()} onClick={openAddButton}>
                  {t("layout.addButton")}
                </Button>
                <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <Show
              when={buttons()}
              fallback={<p class="text-sm text-ink-muted">{t("layout.loadHint")}</p>}
            >
              <div class="flex flex-col gap-5">
                <Show when={collisionCells().size > 0}>
                  <Banner
                    tone="danger"
                    message={t("layout.collision", { count: collisionCells().size })}
                  />
                </Show>

                {/* Copy this channel's buttons to another channel. */}
                <div class="flex flex-wrap items-end gap-2">
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("layout.copyTarget")}
                    </span>
                    <select
                      class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                      value={copyTarget()}
                      onChange={(event) =>
                        setCopyTarget(event.currentTarget.value as SalesChannel | "")
                      }
                    >
                      <option value="">{t("layout.copyChooseTarget")}</option>
                      <For each={SALES_CHANNELS.filter((row) => row !== channel())}>
                        {(row) => <option value={row}>{t(CHANNEL_LABEL[row])}</option>}
                      </For>
                    </select>
                  </label>
                  <Button
                    variant="secondary"
                    disabled={busy() || !copyTarget() || channelButtons().length === 0}
                    onClick={() => setCopyConfirm(true)}
                  >
                    {t("layout.copyAction")}
                  </Button>
                </div>

                {/* Device-shaped grid preview of the positioned buttons. */}
                <div>
                  <h3 class="mb-2 text-base font-semibold text-ink">{t("layout.gridPreview")}</h3>
                  <Show
                    when={positionedButtons().length > 0}
                    fallback={<p class="text-sm text-ink-muted">{t("layout.gridEmpty")}</p>}
                  >
                    <div class="w-fit max-w-full overflow-x-auto rounded-token border-4 border-line bg-surface-raised p-3">
                      <div
                        class="grid gap-2"
                        style={{
                          "grid-template-columns": `repeat(${gridExtent().cols}, minmax(4rem, 1fr))`,
                        }}
                      >
                        <For each={grid()}>
                          {(rowCells) => (
                            <For each={rowCells}>
                              {(cell) => (
                                <Show
                                  when={cell.length > 0}
                                  fallback={
                                    <div class="min-h-touch rounded-token border border-dashed border-line" />
                                  }
                                >
                                  <div
                                    class={`flex min-h-touch flex-col gap-1 rounded-token border p-1 ${
                                      cell.length > 1 ? "border-danger" : "border-accent"
                                    }`}
                                  >
                                    <For each={cell}>
                                      {(button) => (
                                        <button
                                          type="button"
                                          disabled={busy()}
                                          class="rounded-token px-1 text-left text-sm text-ink hover:underline"
                                          onClick={() => openEditButton(button)}
                                        >
                                          {button.label}
                                        </button>
                                      )}
                                    </For>
                                  </div>
                                </Show>
                              )}
                            </For>
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>
                </div>

                {/* Flowing buttons (no grid slot) — ordered by the kit's ReorderList. */}
                <div>
                  <h3 class="mb-1 text-base font-semibold text-ink">{t("layout.flowingButtons")}</h3>
                  <p class="mb-2 text-xs text-ink-muted">{t("layout.flowingHint")}</p>
                  <Show
                    when={flowingButtons().length > 0}
                    fallback={<p class="text-sm text-ink-muted">{t("layout.flowingEmpty")}</p>}
                  >
                    <ReorderList
                      items={flowingButtons()}
                      itemKey={(button) => button.menu_item_id}
                      upLabel={t("layout.moveUp")}
                      downLabel={t("layout.moveDown")}
                      onReorder={(from, to) => void reorderFlowing(from, to)}
                      renderItem={(button) => (
                        <div class="flex flex-wrap items-center justify-between gap-2">
                          <div class="flex flex-col">
                            <span class="text-sm text-ink">{button.label}</span>
                            <span class="text-xs text-ink-muted">
                              {itemName(button.menu_item_id)} · {categoryName(button.display_category_id)}
                            </span>
                          </div>
                          <div class="flex flex-wrap gap-2">
                            <Button
                              variant="secondary"
                              disabled={busy()}
                              onClick={() => openEditButton(button)}
                            >
                              {t("action.edit")}
                            </Button>
                            <Button
                              variant="danger"
                              disabled={busy()}
                              onClick={() => setPendingRemove(button)}
                            >
                              {t("catalog.remove")}
                            </Button>
                          </div>
                        </div>
                      )}
                    />
                  </Show>
                </div>
              </div>
            </Show>
          </Card>

          <div class="grid gap-6 lg:grid-cols-2">
            <Card
              title={t("layout.categories")}
              actions={
                <Button disabled={busy()} onClick={openCreateCategory}>
                  {t("action.create")}
                </Button>
              }
            >
              <Show
                when={categories()}
                fallback={<p class="text-sm text-ink-muted">{t("layout.loadHint")}</p>}
              >
                {(loaded) => (
                  <DataTable
                    columns={categoryColumns()}
                    rows={loaded()}
                    searchText={(row) => row.name}
                    empty={<EmptyState title={t("layout.categoriesEmpty")} />}
                    actionsHeader={t("common.actions")}
                    actions={(row) => (
                      <div class="flex flex-wrap gap-2">
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => openEditCategory(row)}
                        >
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
              title={t("layout.subcategories")}
              actions={
                <Button disabled={busy()} onClick={openCreateSubcategory}>
                  {t("action.create")}
                </Button>
              }
            >
              <Show
                when={subcategories()}
                fallback={<p class="text-sm text-ink-muted">{t("layout.loadHint")}</p>}
              >
                {(loaded) => (
                  <DataTable
                    columns={subcategoryColumns()}
                    rows={loaded()}
                    searchText={(row) => `${row.name} ${categoryName(row.display_category_id)}`}
                    empty={<EmptyState title={t("layout.subcategoriesEmpty")} />}
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
          </div>
        </div>

        {/* Button editor */}
        <Drawer
          open={buttonOpen()}
          title={buttonEditing() ? t("layout.editButton") : t("layout.addButton")}
          closeLabel={t("action.close")}
          onClose={() => setButtonOpen(false)}
          footer={
            <>
              <Show when={buttonEditing() !== null}>
                <Button
                  variant="danger"
                  disabled={busy()}
                  onClick={() => {
                    const current = channelButtons().find(
                      (button) => button.menu_item_id === buttonItem(),
                    );
                    if (current) {
                      setPendingRemove(current);
                    }
                  }}
                >
                  {t("catalog.remove")}
                </Button>
              </Show>
              <Button variant="secondary" onClick={() => setButtonOpen(false)}>
                {t("action.cancel")}
              </Button>
              <Button disabled={busy()} onClick={() => void saveButton()}>
                {t("layout.saveButton")}
              </Button>
            </>
          }
        >
          <div class="flex flex-col gap-4">
            <FormField label={t("layout.item")}>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-50"
                value={buttonItem()}
                disabled={buttonEditing() !== null}
                onChange={(event) => setButtonItem(event.currentTarget.value)}
              >
                <option value="">{t("layout.chooseItem")}</option>
                <For each={items().filter((item) => item.status === "active")}>
                  {(item) => <option value={item.menu_item_id}>{item.name}</option>}
                </For>
              </select>
            </FormField>
            <TextField
              label={t("layout.label")}
              value={buttonLabel()}
              onInput={setButtonLabel}
              placeholder={t("layout.labelPlaceholder")}
            />
            <FormField label={t("layout.category")}>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                value={buttonCategory()}
                onChange={(event) => {
                  setButtonCategory(event.currentTarget.value);
                  setButtonSubcategory("");
                }}
              >
                <option value="">{t("layout.chooseCategory")}</option>
                <For each={activeCategories()}>
                  {(row) => <option value={row.display_category_id}>{row.name}</option>}
                </For>
              </select>
            </FormField>
            <FormField label={t("layout.subcategory")}>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-50"
                value={buttonSubcategory()}
                disabled={!buttonCategory()}
                onChange={(event) => setButtonSubcategory(event.currentTarget.value)}
              >
                <option value="">{t("layout.noSubcategory")}</option>
                <For each={activeSubcategories(buttonCategory())}>
                  {(row) => <option value={row.display_subcategory_id}>{row.name}</option>}
                </For>
              </select>
            </FormField>
            <p class="text-xs text-ink-muted">{t("layout.gridHint")}</p>
            <div class="grid grid-cols-2 gap-3">
              <FormField label={t("layout.gridColumn")}>
                <input
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  inputmode="numeric"
                  aria-label={t("layout.gridColumn")}
                  value={buttonColumn()}
                  onInput={(event) => setButtonColumn(event.currentTarget.value)}
                />
              </FormField>
              <FormField label={t("layout.gridRow")}>
                <input
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  inputmode="numeric"
                  aria-label={t("layout.gridRow")}
                  value={buttonRow()}
                  onInput={(event) => setButtonRow(event.currentTarget.value)}
                />
              </FormField>
            </div>
          </div>
        </Drawer>

        {/* Category create / edit */}
        <Drawer
          open={creatingCategory()}
          title={t("layout.categoryName")}
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
            label={t("layout.categoryName")}
            value={newCategoryName()}
            onInput={setNewCategoryName}
            placeholder={t("layout.categoryNamePlaceholder")}
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
          <TextField label={t("layout.categoryName")} value={draftName()} onInput={setDraftName} />
        </Drawer>

        {/* Sub-category create / edit */}
        <Drawer
          open={creatingSubcategory()}
          title={t("layout.subcategoryName")}
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
            <FormField label={t("layout.parentCategory")}>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                value={newSubcategoryParent()}
                onChange={(event) => setNewSubcategoryParent(event.currentTarget.value)}
              >
                <option value="">{t("layout.chooseCategory")}</option>
                <For each={activeCategories()}>
                  {(row) => <option value={row.display_category_id}>{row.name}</option>}
                </For>
              </select>
            </FormField>
            <TextField
              label={t("layout.subcategoryName")}
              value={newSubcategoryName()}
              onInput={setNewSubcategoryName}
              placeholder={t("layout.subcategoryNamePlaceholder")}
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
          <TextField
            label={t("layout.subcategoryName")}
            value={draftName()}
            onInput={setDraftName}
          />
        </Drawer>

        <ConfirmDialog
          open={pendingRemove() !== null}
          title={t("layout.removeTitle")}
          message={t("layout.removeMessage")}
          confirmLabel={t("catalog.remove")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void removeButton()}
          onCancel={() => setPendingRemove(null)}
        />

        <ConfirmDialog
          open={copyConfirm()}
          title={t("layout.copyTitle")}
          message={t("layout.copyMessage", {
            from: t(CHANNEL_LABEL[channel()]),
            to: copyTarget() ? t(CHANNEL_LABEL[copyTarget() as SalesChannel]) : "",
          })}
          confirmLabel={t("layout.copyAction")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={() => void doCopy()}
          onCancel={() => setCopyConfirm(false)}
        />
      </RequireContext>
    </div>
  );
}
