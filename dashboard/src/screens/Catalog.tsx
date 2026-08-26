// The menu editor (ADR-0066, Phase 2a). The operator's place to author the catalog the edge reprices
// from — items (the product master), menus that inherit from one another, and the per-channel prices
// an item carries in a menu — and to publish a menu to a store, which compiles it to a `MenuBook` and
// writes it onto the store's config tree. Everything is by name; tenant comes from the top-bar
// context, and the store to publish to is the store in context. Prices are a T2 asset — authored here
// and shipped in config, never logged.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type {
  CatalogItem,
  ChannelPrice,
  EntityStatus,
  ItemCategory,
  ItemSubcategory,
  Menu,
  MenuPlacement,
  MenuSection,
  ModifierGroup,
  SalesChannel,
  TaxClass,
} from "../api/types";
import { SALES_CHANNELS } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { formatMoney } from "../lib/format";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

// A blank per-channel price sheet: every channel maps to an empty amount string (not priced).
const emptyPriceSheet = (): Record<SalesChannel, string> =>
  Object.fromEntries(SALES_CHANNELS.map((channel) => [channel, ""])) as Record<
    SalesChannel,
    string
  >;

const CHANNEL_LABEL: Record<SalesChannel, MessageKey> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

export function Catalog() {
  const [items, setItems] = createSignal<CatalogItem[] | null>(null);
  const [taxClasses, setTaxClasses] = createSignal<TaxClass[]>([]);
  const [categories, setCategories] = createSignal<ItemCategory[]>([]);
  const [subcategories, setSubcategories] = createSignal<ItemSubcategory[]>([]);
  const [modifierGroups, setModifierGroups] = createSignal<ModifierGroup[]>([]);
  const [menus, setMenus] = createSignal<Menu[] | null>(null);
  const [menuSections, setMenuSections] = createSignal<MenuSection[]>([]);
  const [placements, setPlacements] = createSignal<MenuPlacement[] | null>(null);
  const [selectedMenu, setSelectedMenu] = createSignal("");
  const [error, setError] = createSignal("");
  const [notice, setNotice] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // Create forms.
  const [newItemName, setNewItemName] = createSignal("");
  const [newItemTaxClass, setNewItemTaxClass] = createSignal("");
  const [newItemCategory, setNewItemCategory] = createSignal("");
  const [newItemSubcategory, setNewItemSubcategory] = createSignal("");
  const [newTaxClassName, setNewTaxClassName] = createSignal("");
  const [newGroupName, setNewGroupName] = createSignal("");
  const [newGroupMin, setNewGroupMin] = createSignal("0");
  const [newGroupMax, setNewGroupMax] = createSignal("1");
  const [newGroupMembers, setNewGroupMembers] = createSignal<string[]>([]);
  const [newGroupAttached, setNewGroupAttached] = createSignal<string[]>([]);
  const [editingGroup, setEditingGroup] = createSignal("");
  const [newCategoryName, setNewCategoryName] = createSignal("");
  const [newSubcategoryName, setNewSubcategoryName] = createSignal("");
  const [newSubcategoryParent, setNewSubcategoryParent] = createSignal("");
  const [newMenuName, setNewMenuName] = createSignal("");
  const [newMenuParent, setNewMenuParent] = createSignal("");
  const [newSectionName, setNewSectionName] = createSignal("");
  const [newSectionSort, setNewSectionSort] = createSignal("0");

  // Inline rename.
  const [editingItem, setEditingItem] = createSignal("");
  const [editingTaxClass, setEditingTaxClass] = createSignal("");
  const [editingCategory, setEditingCategory] = createSignal("");
  const [editingSubcategory, setEditingSubcategory] = createSignal("");
  const [editingMenu, setEditingMenu] = createSignal("");
  const [editingSection, setEditingSection] = createSignal("");
  const [draftSectionSort, setDraftSectionSort] = createSignal("0");
  const [draftName, setDraftName] = createSignal("");

  // Placement editor.
  const [placementItem, setPlacementItem] = createSignal("");
  const [placementSection, setPlacementSection] = createSignal("");
  const [placementCurrency, setPlacementCurrency] = createSignal("VND");
  const [placementAvailable, setPlacementAvailable] = createSignal(true);
  const [priceSheet, setPriceSheet] = createSignal(emptyPriceSheet());
  const [placementEditing, setPlacementEditing] = createSignal(false);

  // Publish.
  const [publishMenu, setPublishMenu] = createSignal("");

  const fail = (caught: unknown) => {
    setNotice("");
    setError(caught instanceof ApiError ? caught.message : String(caught));
  };

  const itemName = (id: string) =>
    items()?.find((item) => item.menu_item_id === id)?.name ?? id;
  const menuName = (id: string) => menus()?.find((menu) => menu.menu_id === id)?.name ?? id;
  const sectionName = (id: string | null) =>
    id ? (menuSections().find((row) => row.menu_section_id === id)?.name ?? id) : "—";
  const taxClassName = (id: string) =>
    taxClasses().find((row) => row.tax_class_id === id)?.name ?? id;
  const categoryName = (id: string | null) =>
    id ? (categories().find((row) => row.item_category_id === id)?.name ?? id) : "—";

  const load = async () => {
    setError("");
    setNotice("");
    setBusy(true);
    try {
      const [
        loadedItems,
        loadedTaxClasses,
        loadedCategories,
        loadedSubcategories,
        loadedGroups,
        loadedMenus,
      ] = await Promise.all([
        api.listItems(tenantId()),
        api.listTaxClasses(tenantId()),
        api.listItemCategories(tenantId()),
        api.listItemSubcategories(tenantId()),
        api.listModifierGroups(tenantId()),
        api.listMenus(tenantId()),
      ]);
      setItems(loadedItems);
      setTaxClasses(loadedTaxClasses);
      setCategories(loadedCategories);
      setSubcategories(loadedSubcategories);
      setModifierGroups(loadedGroups);
      setMenus(loadedMenus);
      if (selectedMenu()) {
        await loadPlacements(selectedMenu());
      }
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const selectedValues = (select: HTMLSelectElement): string[] =>
    Array.from(select.selectedOptions, (option) => option.value);

  const createGroup = async () => {
    const name = newGroupName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    const min = Number(newGroupMin().trim() || "0");
    const max = Number(newGroupMax().trim() || "0");
    if (!Number.isInteger(min) || min < 0 || !Number.isInteger(max) || max < min) {
      setError(t("catalog.groupRuleInvalid"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createModifierGroup(tenantId(), {
        name,
        minSelect: min,
        maxSelect: max,
        memberItemIds: newGroupMembers(),
        attachedItemIds: newGroupAttached(),
      });
      setNewGroupName("");
      setNewGroupMin("0");
      setNewGroupMax("1");
      setNewGroupMembers([]);
      setNewGroupAttached([]);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setGroupFields = async (
    group: ModifierGroup,
    fields: { name?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? group.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateModifierGroup(group.modifier_group_id, tenantId(), {
        name,
        minSelect: group.min_select,
        maxSelect: group.max_select,
        memberItemIds: group.member_item_ids,
        attachedItemIds: group.attached_item_ids,
        status: fields.status ?? group.status,
      });
      setEditingGroup("");
      setDraftName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const createCategory = async () => {
    const name = newCategoryName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createItemCategory(tenantId(), name);
      setNewCategoryName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setCategoryFields = async (
    category: ItemCategory,
    fields: { name?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? category.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateItemCategory(category.item_category_id, tenantId(), {
        name,
        status: fields.status ?? category.status,
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
      setError(t("catalog.parentCategoryRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createItemSubcategory(tenantId(), newSubcategoryParent(), name);
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
    subcategory: ItemSubcategory,
    fields: { name?: string; itemCategoryId?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? subcategory.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateItemSubcategory(subcategory.item_subcategory_id, tenantId(), {
        itemCategoryId: fields.itemCategoryId ?? subcategory.item_category_id,
        name,
        status: fields.status ?? subcategory.status,
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

  const createTaxClass = async () => {
    const name = newTaxClassName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createTaxClass(tenantId(), name);
      setNewTaxClassName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setTaxClassFields = async (
    taxClass: TaxClass,
    fields: { name?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? taxClass.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateTaxClass(taxClass.tax_class_id, tenantId(), {
        name,
        status: fields.status ?? taxClass.status,
      });
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const loadPlacements = async (menuId: string) => {
    const [loadedPlacements, loadedSections] = await Promise.all([
      api.listPlacements(tenantId(), menuId),
      api.listMenuSections(tenantId(), menuId),
    ]);
    setPlacements(loadedPlacements);
    setMenuSections(loadedSections);
  };

  const openMenu = async (menuId: string) => {
    setSelectedMenu(menuId);
    resetPlacementEditor();
    setError("");
    setBusy(true);
    try {
      await loadPlacements(menuId);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- items ---

  const createItem = async () => {
    const name = newItemName().trim();
    const taxClass = newItemTaxClass().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    if (!taxClass) {
      setError(t("catalog.taxClassRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createItem(tenantId(), name, taxClass, {
        itemCategoryId: newItemCategory() || null,
        itemSubcategoryId: newItemSubcategory() || null,
      });
      setNewItemName("");
      setNewItemTaxClass("");
      setNewItemCategory("");
      setNewItemSubcategory("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setItemFields = async (
    item: CatalogItem,
    fields: { name?: string; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? item.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateItem(item.menu_item_id, tenantId(), {
        name,
        taxClassId: item.tax_class_id,
        itemCategoryId: item.item_category_id,
        itemSubcategoryId: item.item_subcategory_id,
        status: fields.status ?? item.status,
      });
      setEditingItem("");
      setDraftName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- menus ---

  const createMenu = async () => {
    const name = newMenuName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createMenu(tenantId(), name, newMenuParent() || undefined);
      setNewMenuName("");
      setNewMenuParent("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setMenuFields = async (
    menu: Menu,
    fields: { name?: string; parentMenuId?: string | null; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? menu.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateMenu(menu.menu_id, tenantId(), {
        name,
        parentMenuId:
          fields.parentMenuId === undefined ? menu.parent_menu_id : fields.parentMenuId,
        status: fields.status ?? menu.status,
      });
      setEditingMenu("");
      setDraftName("");
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- menu sections (authoring groupings within the selected menu) ---

  const createMenuSection = async () => {
    const name = newSectionName().trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    const sort = Number(newSectionSort());
    if (!Number.isInteger(sort)) {
      setError(t("catalog.sortInvalid"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createMenuSection(tenantId(), selectedMenu(), name, sort);
      setNewSectionName("");
      setNewSectionSort("0");
      await loadPlacements(selectedMenu());
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setSectionFields = async (
    section: MenuSection,
    fields: { name?: string; sort?: number; status?: EntityStatus },
  ) => {
    const name = (fields.name ?? section.name).trim();
    if (!name) {
      setError(t("catalog.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.updateMenuSection(tenantId(), selectedMenu(), section.menu_section_id, {
        name,
        sort: fields.sort ?? section.sort,
        status: fields.status ?? section.status,
      });
      setEditingSection("");
      setDraftName("");
      setDraftSectionSort("0");
      await loadPlacements(selectedMenu());
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // --- placements ---

  const resetPlacementEditor = () => {
    setPlacementItem("");
    setPlacementSection("");
    setPlacementCurrency("VND");
    setPlacementAvailable(true);
    setPriceSheet(emptyPriceSheet());
    setPlacementEditing(false);
  };

  const editPlacement = (placement: MenuPlacement) => {
    const sheet = emptyPriceSheet();
    let currency = "VND";
    for (const price of placement.prices) {
      if (price.sales_channel) {
        sheet[price.sales_channel] = String(price.unit_price.amount_minor);
        currency = price.unit_price.currency_code;
      }
    }
    setPlacementItem(placement.menu_item_id);
    setPlacementSection(placement.menu_section_id ?? "");
    setPlacementCurrency(currency);
    setPlacementAvailable(placement.available);
    setPriceSheet(sheet);
    setPlacementEditing(true);
  };

  const setChannelAmount = (channel: SalesChannel, value: string) =>
    setPriceSheet({ ...priceSheet(), [channel]: value });

  const savePlacement = async () => {
    const item = placementItem();
    if (!item) {
      setError(t("catalog.itemRequired"));
      return;
    }
    const currency = placementCurrency().trim() || "VND";
    const prices: ChannelPrice[] = [];
    for (const channel of SALES_CHANNELS) {
      const raw = priceSheet()[channel].trim();
      if (!raw) {
        continue;
      }
      const amount = Number(raw);
      if (!Number.isInteger(amount) || amount < 0) {
        setError(t("catalog.priceInvalid"));
        return;
      }
      prices.push({ sales_channel: channel, unit_price: { currency_code: currency, amount_minor: amount } });
    }
    setError("");
    setBusy(true);
    try {
      await api.setPlacement(
        tenantId(),
        selectedMenu(),
        item,
        prices,
        placementAvailable(),
        placementSection() || null,
      );
      resetPlacementEditor();
      await loadPlacements(selectedMenu());
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removePlacement = async (menuItemId: string) => {
    setError("");
    setBusy(true);
    try {
      await api.deletePlacement(tenantId(), selectedMenu(), menuItemId);
      await loadPlacements(selectedMenu());
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const priceSummary = (placement: MenuPlacement) => {
    const parts = placement.prices
      .filter((price) => price.sales_channel)
      .map(
        (price) =>
          `${t(CHANNEL_LABEL[price.sales_channel as SalesChannel])} ${formatMoney(price.unit_price)}`,
      );
    return parts.length > 0 ? parts.join(" · ") : t("catalog.noPrices");
  };

  // --- publish ---

  const doPublish = async () => {
    if (!publishMenu()) {
      setError(t("catalog.menuRequired"));
      return;
    }
    if (!storeId()) {
      setError(t("context.storeRequired"));
      return;
    }
    setError("");
    setNotice("");
    setBusy(true);
    try {
      const result = await api.publishMenu(tenantId(), storeId(), publishMenu());
      setNotice(t("catalog.published", { version: result.config_version_id }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const statusLabel = (status: EntityStatus) =>
    status === "archived" ? t("catalog.statusArchived") : t("catalog.statusActive");

  return (
    <div>
      <PageHeader title={t("catalog.title")} description={t("catalog.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={notice()}>{(message) => <Banner tone="ok" message={message()} />}</Show>

          <Card
            title={t("catalog.items")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={items()}
              fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
            >
              {(loaded) => (
                <Show
                  when={loaded().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("catalog.itemsEmpty")}</p>}
                >
                  <div class="overflow-x-auto">
                    <table class="w-full text-left text-sm">
                      <thead>
                        <tr class="border-b border-line text-ink-muted">
                          <th class="py-2 pr-4 font-medium">{t("catalog.name")}</th>
                          <th class="py-2 pr-4 font-medium">{t("catalog.taxClass")}</th>
                          <th class="py-2 pr-4 font-medium">{t("catalog.category")}</th>
                          <th class="py-2 pr-4 font-medium">{t("catalog.status")}</th>
                          <th class="py-2 font-medium">{t("catalog.actions")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={loaded()}>
                          {(item) => (
                            <tr class="border-b border-line align-top text-ink">
                              <td class="py-2 pr-4">
                                <Show
                                  when={editingItem() === item.menu_item_id}
                                  fallback={
                                    <div class="flex flex-col">
                                      <span>{item.name}</span>
                                      <span class="font-mono text-xs text-ink-muted">
                                        {item.menu_item_id}
                                      </span>
                                    </div>
                                  }
                                >
                                  <div class="flex flex-wrap items-center gap-2">
                                    <input
                                      class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                      aria-label={t("catalog.name")}
                                      value={draftName()}
                                      onInput={(event) => setDraftName(event.currentTarget.value)}
                                    />
                                    <Button
                                      disabled={busy()}
                                      onClick={() => void setItemFields(item, { name: draftName() })}
                                    >
                                      {t("action.save")}
                                    </Button>
                                    <Button
                                      variant="secondary"
                                      onClick={() => {
                                        setEditingItem("");
                                        setDraftName("");
                                      }}
                                    >
                                      {t("action.cancel")}
                                    </Button>
                                  </div>
                                </Show>
                              </td>
                              <td class="py-2 pr-4">{taxClassName(item.tax_class_id)}</td>
                              <td class="py-2 pr-4">{categoryName(item.item_category_id)}</td>
                              <td class="py-2 pr-4">{statusLabel(item.status)}</td>
                              <td class="flex flex-wrap gap-2 py-2">
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingItem(item.menu_item_id);
                                    setDraftName(item.name);
                                  }}
                                >
                                  {t("catalog.rename")}
                                </Button>
                                <Button
                                  variant={item.status === "archived" ? "secondary" : "danger"}
                                  disabled={busy()}
                                  onClick={() =>
                                    void setItemFields(item, {
                                      status: item.status === "archived" ? "active" : "archived",
                                    })
                                  }
                                >
                                  {item.status === "archived"
                                    ? t("catalog.restore")
                                    : t("catalog.archive")}
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

          <Card title={t("catalog.createItem")}>
            <div class="grid gap-4 md:grid-cols-2 md:items-end">
              <TextField
                label={t("catalog.name")}
                value={newItemName()}
                onInput={setNewItemName}
                placeholder={t("catalog.namePlaceholder")}
              />
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">{t("catalog.taxClass")}</span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={newItemTaxClass()}
                  onChange={(event) => setNewItemTaxClass(event.currentTarget.value)}
                >
                  <option value="">{t("catalog.chooseTaxClass")}</option>
                  <For each={taxClasses().filter((row) => row.status === "active")}>
                    {(row) => <option value={row.tax_class_id}>{row.name}</option>}
                  </For>
                </select>
                <Show when={taxClasses().filter((row) => row.status === "active").length === 0}>
                  <p class="mt-1 text-xs text-ink-muted">{t("catalog.taxClassEmpty")}</p>
                </Show>
              </label>
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">{t("catalog.category")}</span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={newItemCategory()}
                  onChange={(event) => {
                    setNewItemCategory(event.currentTarget.value);
                    setNewItemSubcategory("");
                  }}
                >
                  <option value="">{t("catalog.noCategory")}</option>
                  <For each={categories().filter((row) => row.status === "active")}>
                    {(row) => <option value={row.item_category_id}>{row.name}</option>}
                  </For>
                </select>
              </label>
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">
                  {t("catalog.subcategory")}
                </span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-50"
                  value={newItemSubcategory()}
                  disabled={!newItemCategory()}
                  onChange={(event) => setNewItemSubcategory(event.currentTarget.value)}
                >
                  <option value="">{t("catalog.noSubcategory")}</option>
                  <For
                    each={subcategories().filter(
                      (row) =>
                        row.status === "active" && row.item_category_id === newItemCategory(),
                    )}
                  >
                    {(row) => <option value={row.item_subcategory_id}>{row.name}</option>}
                  </For>
                </select>
              </label>
              <Button disabled={busy()} onClick={() => void createItem()}>
                {t("action.create")}
              </Button>
            </div>
          </Card>

          <div class="grid gap-6 lg:grid-cols-2">
            <Card title={t("catalog.categories")}>
              <div class="flex flex-col gap-4">
                <Show
                  when={categories().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("catalog.categoriesEmpty")}</p>}
                >
                  <ul class="flex flex-col gap-2">
                    <For each={categories()}>
                      {(row) => (
                        <li class="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2 text-sm text-ink">
                          <Show
                            when={editingCategory() === row.item_category_id}
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
                              aria-label={t("catalog.name")}
                              value={draftName()}
                              onInput={(event) => setDraftName(event.currentTarget.value)}
                            />
                          </Show>
                          <div class="flex flex-wrap gap-2">
                            <Show
                              when={editingCategory() === row.item_category_id}
                              fallback={
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingCategory(row.item_category_id);
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
                    label={t("catalog.categoryName")}
                    value={newCategoryName()}
                    onInput={setNewCategoryName}
                    placeholder={t("catalog.categoryNamePlaceholder")}
                  />
                  <Button disabled={busy()} onClick={() => void createCategory()}>
                    {t("action.create")}
                  </Button>
                </div>
              </div>
            </Card>

            <Card title={t("catalog.subcategories")}>
              <div class="flex flex-col gap-4">
                <Show
                  when={subcategories().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("catalog.subcategoriesEmpty")}</p>}
                >
                  <ul class="flex flex-col gap-2">
                    <For each={subcategories()}>
                      {(row) => (
                        <li class="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2 text-sm text-ink">
                          <Show
                            when={editingSubcategory() === row.item_subcategory_id}
                            fallback={
                              <span>
                                {row.name}
                                <span class="ml-2 text-xs text-ink-muted">
                                  {categoryName(row.item_category_id)}
                                </span>
                              </span>
                            }
                          >
                            <input
                              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                              aria-label={t("catalog.name")}
                              value={draftName()}
                              onInput={(event) => setDraftName(event.currentTarget.value)}
                            />
                          </Show>
                          <div class="flex flex-wrap gap-2">
                            <Show
                              when={editingSubcategory() === row.item_subcategory_id}
                              fallback={
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingSubcategory(row.item_subcategory_id);
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
                      {t("catalog.parentCategory")}
                    </span>
                    <select
                      class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                      value={newSubcategoryParent()}
                      onChange={(event) => setNewSubcategoryParent(event.currentTarget.value)}
                    >
                      <option value="">{t("catalog.chooseCategory")}</option>
                      <For each={categories().filter((row) => row.status === "active")}>
                        {(row) => <option value={row.item_category_id}>{row.name}</option>}
                      </For>
                    </select>
                  </label>
                  <TextField
                    label={t("catalog.subcategoryName")}
                    value={newSubcategoryName()}
                    onInput={setNewSubcategoryName}
                    placeholder={t("catalog.subcategoryNamePlaceholder")}
                  />
                  <Button disabled={busy()} onClick={() => void createSubcategory()}>
                    {t("action.create")}
                  </Button>
                </div>
              </div>
            </Card>
          </div>

          <Card title={t("catalog.taxClasses")}>
            <div class="flex flex-col gap-4">
              <Show
                when={taxClasses().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("catalog.taxClassesEmpty")}</p>}
              >
                <div class="overflow-x-auto">
                  <table class="w-full text-left text-sm">
                    <thead>
                      <tr class="border-b border-line text-ink-muted">
                        <th class="py-2 pr-4 font-medium">{t("catalog.name")}</th>
                        <th class="py-2 pr-4 font-medium">{t("catalog.status")}</th>
                        <th class="py-2 font-medium">{t("catalog.actions")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={taxClasses()}>
                        {(row) => (
                          <tr class="border-b border-line align-top text-ink">
                            <td class="py-2 pr-4">
                              <Show
                                when={editingTaxClass() === row.tax_class_id}
                                fallback={<span>{row.name}</span>}
                              >
                                <div class="flex flex-wrap items-center gap-2">
                                  <input
                                    class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                    aria-label={t("catalog.name")}
                                    value={draftName()}
                                    onInput={(event) => setDraftName(event.currentTarget.value)}
                                  />
                                  <Button
                                    disabled={busy()}
                                    onClick={() =>
                                      void setTaxClassFields(row, { name: draftName() })
                                    }
                                  >
                                    {t("action.save")}
                                  </Button>
                                  <Button
                                    variant="secondary"
                                    onClick={() => {
                                      setEditingTaxClass("");
                                      setDraftName("");
                                    }}
                                  >
                                    {t("action.cancel")}
                                  </Button>
                                </div>
                              </Show>
                            </td>
                            <td class="py-2 pr-4">{statusLabel(row.status)}</td>
                            <td class="flex flex-wrap gap-2 py-2">
                              <Button
                                variant="secondary"
                                disabled={busy()}
                                onClick={() => {
                                  setEditingTaxClass(row.tax_class_id);
                                  setDraftName(row.name);
                                }}
                              >
                                {t("catalog.rename")}
                              </Button>
                              <Button
                                variant={row.status === "archived" ? "secondary" : "danger"}
                                disabled={busy()}
                                onClick={() =>
                                  void setTaxClassFields(row, {
                                    status: row.status === "archived" ? "active" : "archived",
                                  })
                                }
                              >
                                {row.status === "archived"
                                  ? t("catalog.restore")
                                  : t("catalog.archive")}
                              </Button>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>
              <div class="grid gap-4 md:grid-cols-2 md:items-end">
                <TextField
                  label={t("catalog.taxClassName")}
                  value={newTaxClassName()}
                  onInput={setNewTaxClassName}
                  placeholder={t("catalog.taxClassNamePlaceholder")}
                />
                <Button disabled={busy()} onClick={() => void createTaxClass()}>
                  {t("action.create")}
                </Button>
              </div>
            </div>
          </Card>

          <Card title={t("catalog.modifierGroups")}>
            <div class="flex flex-col gap-4">
              <p class="text-sm text-ink-muted">{t("catalog.modifierGroupsHint")}</p>
              <Show
                when={modifierGroups().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("catalog.modifierGroupsEmpty")}</p>}
              >
                <div class="overflow-x-auto">
                  <table class="w-full text-left text-sm">
                    <thead>
                      <tr class="border-b border-line text-ink-muted">
                        <th class="py-2 pr-4 font-medium">{t("catalog.name")}</th>
                        <th class="py-2 pr-4 font-medium">{t("catalog.groupRule")}</th>
                        <th class="py-2 pr-4 font-medium">{t("catalog.groupMembers")}</th>
                        <th class="py-2 pr-4 font-medium">{t("catalog.groupAttached")}</th>
                        <th class="py-2 pr-4 font-medium">{t("catalog.status")}</th>
                        <th class="py-2 font-medium">{t("catalog.actions")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={modifierGroups()}>
                        {(group) => (
                          <tr class="border-b border-line align-top text-ink">
                            <td class="py-2 pr-4">
                              <Show
                                when={editingGroup() === group.modifier_group_id}
                                fallback={<span>{group.name}</span>}
                              >
                                <div class="flex flex-wrap items-center gap-2">
                                  <input
                                    class="min-h-touch w-40 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                    aria-label={t("catalog.name")}
                                    value={draftName()}
                                    onInput={(event) => setDraftName(event.currentTarget.value)}
                                  />
                                  <Button
                                    disabled={busy()}
                                    onClick={() => void setGroupFields(group, { name: draftName() })}
                                  >
                                    {t("action.save")}
                                  </Button>
                                  <Button
                                    variant="secondary"
                                    onClick={() => {
                                      setEditingGroup("");
                                      setDraftName("");
                                    }}
                                  >
                                    {t("action.cancel")}
                                  </Button>
                                </div>
                              </Show>
                            </td>
                            <td class="py-2 pr-4">
                              {group.min_select}–{group.max_select}
                            </td>
                            <td class="py-2 pr-4">{group.member_item_ids.length}</td>
                            <td class="py-2 pr-4">{group.attached_item_ids.length}</td>
                            <td class="py-2 pr-4">{statusLabel(group.status)}</td>
                            <td class="flex flex-wrap gap-2 py-2">
                              <Button
                                variant="secondary"
                                disabled={busy()}
                                onClick={() => {
                                  setEditingGroup(group.modifier_group_id);
                                  setDraftName(group.name);
                                }}
                              >
                                {t("catalog.rename")}
                              </Button>
                              <Button
                                variant={group.status === "archived" ? "secondary" : "danger"}
                                disabled={busy()}
                                onClick={() =>
                                  void setGroupFields(group, {
                                    status: group.status === "archived" ? "active" : "archived",
                                  })
                                }
                              >
                                {group.status === "archived"
                                  ? t("catalog.restore")
                                  : t("catalog.archive")}
                              </Button>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>

              <div class="rounded-token border border-line bg-surface-raised p-4">
                <h3 class="mb-3 text-base font-semibold text-ink">{t("catalog.createGroup")}</h3>
                <div class="grid gap-4 md:grid-cols-3">
                  <TextField
                    label={t("catalog.name")}
                    value={newGroupName()}
                    onInput={setNewGroupName}
                    placeholder={t("catalog.groupNamePlaceholder")}
                  />
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("catalog.groupMin")}
                    </span>
                    <input
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                      inputmode="numeric"
                      aria-label={t("catalog.groupMin")}
                      value={newGroupMin()}
                      onInput={(event) => setNewGroupMin(event.currentTarget.value)}
                    />
                  </label>
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("catalog.groupMax")}
                    </span>
                    <input
                      class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                      inputmode="numeric"
                      aria-label={t("catalog.groupMax")}
                      value={newGroupMax()}
                      onInput={(event) => setNewGroupMax(event.currentTarget.value)}
                    />
                  </label>
                </div>
                <div class="mt-4 grid gap-4 md:grid-cols-2">
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("catalog.groupMembers")}
                    </span>
                    <select
                      multiple
                      class="w-full rounded-token border border-line bg-surface p-2 text-sm text-ink"
                      size="5"
                      onChange={(event) => setNewGroupMembers(selectedValues(event.currentTarget))}
                    >
                      <For each={items() ?? []}>
                        {(item) => (
                          <option
                            value={item.menu_item_id}
                            selected={newGroupMembers().includes(item.menu_item_id)}
                          >
                            {item.name}
                          </option>
                        )}
                      </For>
                    </select>
                  </label>
                  <label class="block">
                    <span class="mb-1 block text-sm font-medium text-ink">
                      {t("catalog.groupAttached")}
                    </span>
                    <select
                      multiple
                      class="w-full rounded-token border border-line bg-surface p-2 text-sm text-ink"
                      size="5"
                      onChange={(event) => setNewGroupAttached(selectedValues(event.currentTarget))}
                    >
                      <For each={items() ?? []}>
                        {(item) => (
                          <option
                            value={item.menu_item_id}
                            selected={newGroupAttached().includes(item.menu_item_id)}
                          >
                            {item.name}
                          </option>
                        )}
                      </For>
                    </select>
                  </label>
                </div>
                <div class="mt-4">
                  <Button disabled={busy()} onClick={() => void createGroup()}>
                    {t("action.create")}
                  </Button>
                </div>
              </div>
            </div>
          </Card>

          <Card title={t("catalog.menus")}>
            <Show
              when={menus()}
              fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
            >
              {(loaded) => (
                <Show
                  when={loaded().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("catalog.menusEmpty")}</p>}
                >
                  <div class="overflow-x-auto">
                    <table class="w-full text-left text-sm">
                      <thead>
                        <tr class="border-b border-line text-ink-muted">
                          <th class="py-2 pr-4 font-medium">{t("catalog.name")}</th>
                          <th class="py-2 pr-4 font-medium">{t("catalog.parent")}</th>
                          <th class="py-2 pr-4 font-medium">{t("catalog.status")}</th>
                          <th class="py-2 font-medium">{t("catalog.actions")}</th>
                        </tr>
                      </thead>
                      <tbody>
                        <For each={loaded()}>
                          {(menu) => (
                            <tr class="border-b border-line align-top text-ink">
                              <td class="py-2 pr-4">
                                <Show
                                  when={editingMenu() === menu.menu_id}
                                  fallback={
                                    <div class="flex flex-col">
                                      <span>{menu.name}</span>
                                      <span class="font-mono text-xs text-ink-muted">
                                        {menu.menu_id}
                                      </span>
                                    </div>
                                  }
                                >
                                  <div class="flex flex-wrap items-center gap-2">
                                    <input
                                      class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                      aria-label={t("catalog.name")}
                                      value={draftName()}
                                      onInput={(event) => setDraftName(event.currentTarget.value)}
                                    />
                                    <Button
                                      disabled={busy()}
                                      onClick={() => void setMenuFields(menu, { name: draftName() })}
                                    >
                                      {t("action.save")}
                                    </Button>
                                    <Button
                                      variant="secondary"
                                      onClick={() => {
                                        setEditingMenu("");
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
                                  aria-label={t("catalog.parent")}
                                  value={menu.parent_menu_id ?? ""}
                                  onChange={(event) =>
                                    void setMenuFields(menu, {
                                      parentMenuId: event.currentTarget.value || null,
                                    })
                                  }
                                >
                                  <option value="">{t("catalog.noParent")}</option>
                                  <For each={loaded().filter((m) => m.menu_id !== menu.menu_id)}>
                                    {(candidate) => (
                                      <option value={candidate.menu_id}>{candidate.name}</option>
                                    )}
                                  </For>
                                </select>
                              </td>
                              <td class="py-2 pr-4">{statusLabel(menu.status)}</td>
                              <td class="flex flex-wrap gap-2 py-2">
                                <Button
                                  disabled={busy()}
                                  onClick={() => void openMenu(menu.menu_id)}
                                >
                                  {t("catalog.openPlacements")}
                                </Button>
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingMenu(menu.menu_id);
                                    setDraftName(menu.name);
                                  }}
                                >
                                  {t("catalog.rename")}
                                </Button>
                                <Button
                                  variant={menu.status === "archived" ? "secondary" : "danger"}
                                  disabled={busy()}
                                  onClick={() =>
                                    void setMenuFields(menu, {
                                      status: menu.status === "archived" ? "active" : "archived",
                                    })
                                  }
                                >
                                  {menu.status === "archived"
                                    ? t("catalog.restore")
                                    : t("catalog.archive")}
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

          <Card title={t("catalog.createMenu")}>
            <div class="grid gap-4 md:grid-cols-3 md:items-end">
              <TextField
                label={t("catalog.name")}
                value={newMenuName()}
                onInput={setNewMenuName}
                placeholder={t("catalog.menuNamePlaceholder")}
              />
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">{t("catalog.parent")}</span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={newMenuParent()}
                  onChange={(event) => setNewMenuParent(event.currentTarget.value)}
                >
                  <option value="">{t("catalog.noParent")}</option>
                  <For each={menus() ?? []}>
                    {(menu) => <option value={menu.menu_id}>{menu.name}</option>}
                  </For>
                </select>
              </label>
              <Button disabled={busy()} onClick={() => void createMenu()}>
                {t("action.create")}
              </Button>
            </div>
          </Card>

          <Show when={selectedMenu()}>
            <div class="flex flex-col gap-6">
            <Card title={t("catalog.sectionsFor", { menu: menuName(selectedMenu()) })}>
              <div class="flex flex-col gap-4">
                <p class="text-sm text-ink-muted">{t("catalog.sectionsHint")}</p>
                <Show
                  when={menuSections().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("catalog.sectionsEmpty")}</p>}
                >
                  <ul class="flex flex-col gap-2">
                    <For each={menuSections()}>
                      {(row) => (
                        <li class="flex flex-wrap items-center justify-between gap-2 border-b border-line pb-2 text-sm text-ink">
                          <Show
                            when={editingSection() === row.menu_section_id}
                            fallback={
                              <span>
                                {row.name}
                                <span class="ml-2 text-xs text-ink-muted">
                                  ({t("catalog.sortLabel", { sort: String(row.sort) })})
                                </span>
                                <Show when={row.status === "archived"}>
                                  <span class="ml-2 text-xs text-ink-muted">
                                    ({t("catalog.statusArchived")})
                                  </span>
                                </Show>
                              </span>
                            }
                          >
                            <div class="flex flex-wrap gap-2">
                              <input
                                class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                aria-label={t("catalog.name")}
                                value={draftName()}
                                onInput={(event) => setDraftName(event.currentTarget.value)}
                              />
                              <input
                                class="min-h-touch w-20 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                inputmode="numeric"
                                aria-label={t("catalog.sort")}
                                value={draftSectionSort()}
                                onInput={(event) => setDraftSectionSort(event.currentTarget.value)}
                              />
                            </div>
                          </Show>
                          <div class="flex flex-wrap gap-2">
                            <Show
                              when={editingSection() === row.menu_section_id}
                              fallback={
                                <Button
                                  variant="secondary"
                                  disabled={busy()}
                                  onClick={() => {
                                    setEditingSection(row.menu_section_id);
                                    setDraftName(row.name);
                                    setDraftSectionSort(String(row.sort));
                                  }}
                                >
                                  {t("catalog.rename")}
                                </Button>
                              }
                            >
                              <Button
                                disabled={busy()}
                                onClick={() =>
                                  void setSectionFields(row, {
                                    name: draftName(),
                                    sort: Number(draftSectionSort()),
                                  })
                                }
                              >
                                {t("action.save")}
                              </Button>
                              <Button
                                variant="secondary"
                                onClick={() => {
                                  setEditingSection("");
                                  setDraftName("");
                                  setDraftSectionSort("0");
                                }}
                              >
                                {t("action.cancel")}
                              </Button>
                            </Show>
                            <Button
                              variant={row.status === "archived" ? "secondary" : "danger"}
                              disabled={busy()}
                              onClick={() =>
                                void setSectionFields(row, {
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
                <div class="grid gap-4 md:grid-cols-3 md:items-end">
                  <TextField
                    label={t("catalog.sectionName")}
                    value={newSectionName()}
                    onInput={setNewSectionName}
                    placeholder={t("catalog.sectionNamePlaceholder")}
                  />
                  <TextField
                    label={t("catalog.sort")}
                    value={newSectionSort()}
                    onInput={setNewSectionSort}
                    placeholder="0"
                  />
                  <Button disabled={busy()} onClick={() => void createMenuSection()}>
                    {t("action.create")}
                  </Button>
                </div>
              </div>
            </Card>
            <Card title={t("catalog.placementsFor", { menu: menuName(selectedMenu()) })}>
              <div class="flex flex-col gap-4">
                <Show
                  when={placements()}
                  fallback={<p class="text-sm text-ink-muted">{t("catalog.chooseMenuHint")}</p>}
                >
                  {(loaded) => (
                    <Show
                      when={loaded().length > 0}
                      fallback={
                        <p class="text-sm text-ink-muted">{t("catalog.placementsEmpty")}</p>
                      }
                    >
                      <div class="overflow-x-auto">
                        <table class="w-full text-left text-sm">
                          <thead>
                            <tr class="border-b border-line text-ink-muted">
                              <th class="py-2 pr-4 font-medium">{t("catalog.item")}</th>
                              <th class="py-2 pr-4 font-medium">{t("catalog.section")}</th>
                              <th class="py-2 pr-4 font-medium">{t("catalog.prices")}</th>
                              <th class="py-2 pr-4 font-medium">{t("catalog.available")}</th>
                              <th class="py-2 font-medium">{t("catalog.actions")}</th>
                            </tr>
                          </thead>
                          <tbody>
                            <For each={loaded()}>
                              {(placement) => (
                                <tr class="border-b border-line align-top text-ink">
                                  <td class="py-2 pr-4">{itemName(placement.menu_item_id)}</td>
                                  <td class="py-2 pr-4">{sectionName(placement.menu_section_id)}</td>
                                  <td class="py-2 pr-4">{priceSummary(placement)}</td>
                                  <td class="py-2 pr-4">
                                    {placement.available
                                      ? t("catalog.availableYes")
                                      : t("catalog.availableNo")}
                                  </td>
                                  <td class="flex flex-wrap gap-2 py-2">
                                    <Button
                                      variant="secondary"
                                      disabled={busy()}
                                      onClick={() => editPlacement(placement)}
                                    >
                                      {t("catalog.edit")}
                                    </Button>
                                    <Button
                                      variant="danger"
                                      disabled={busy()}
                                      onClick={() => void removePlacement(placement.menu_item_id)}
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
                  )}
                </Show>

                <div class="rounded-token border border-line bg-surface-raised p-4">
                  <h3 class="mb-3 text-base font-semibold text-ink">
                    {placementEditing() ? t("catalog.editPlacement") : t("catalog.addPlacement")}
                  </h3>
                  <div class="grid gap-4 md:grid-cols-2">
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("catalog.item")}
                      </span>
                      <select
                        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-50"
                        value={placementItem()}
                        disabled={placementEditing()}
                        onChange={(event) => setPlacementItem(event.currentTarget.value)}
                      >
                        <option value="">{t("catalog.chooseItem")}</option>
                        <For each={(items() ?? []).filter((item) => item.status === "active")}>
                          {(item) => <option value={item.menu_item_id}>{item.name}</option>}
                        </For>
                      </select>
                    </label>
                    <TextField
                      label={t("catalog.currency")}
                      value={placementCurrency()}
                      onInput={setPlacementCurrency}
                      placeholder={t("catalog.currencyPlaceholder")}
                    />
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("catalog.section")}
                      </span>
                      <select
                        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        value={placementSection()}
                        onChange={(event) => setPlacementSection(event.currentTarget.value)}
                      >
                        <option value="">{t("catalog.sectionNone")}</option>
                        <For
                          each={menuSections().filter((row) => row.status === "active")}
                        >
                          {(row) => <option value={row.menu_section_id}>{row.name}</option>}
                        </For>
                      </select>
                    </label>
                  </div>
                  <p class="mt-3 mb-2 text-sm font-medium text-ink">{t("catalog.prices")}</p>
                  <p class="mb-3 text-xs text-ink-muted">{t("catalog.pricesHint")}</p>
                  <div class="grid gap-3 md:grid-cols-2 lg:grid-cols-3">
                    <For each={SALES_CHANNELS}>
                      {(channel) => (
                        <label class="block">
                          <span class="mb-1 block text-sm text-ink">{t(CHANNEL_LABEL[channel])}</span>
                          <input
                            class="min-h-touch w-full rounded-token border border-line bg-surface px-3 text-base text-ink"
                            inputmode="numeric"
                            aria-label={t(CHANNEL_LABEL[channel])}
                            value={priceSheet()[channel]}
                            onInput={(event) =>
                              setChannelAmount(channel, event.currentTarget.value)
                            }
                          />
                        </label>
                      )}
                    </For>
                  </div>
                  <label class="mt-4 flex items-center gap-2 text-sm text-ink">
                    <input
                      type="checkbox"
                      class="size-5"
                      checked={placementAvailable()}
                      onChange={(event) => setPlacementAvailable(event.currentTarget.checked)}
                    />
                    {t("catalog.availableLabel")}
                  </label>
                  <div class="mt-4 flex flex-wrap gap-2">
                    <Button disabled={busy()} onClick={() => void savePlacement()}>
                      {t("catalog.savePlacement")}
                    </Button>
                    <Show when={placementEditing()}>
                      <Button variant="secondary" onClick={resetPlacementEditor}>
                        {t("action.cancel")}
                      </Button>
                    </Show>
                  </div>
                </div>
              </div>
            </Card>
            </div>
          </Show>

          <Card title={t("catalog.publish")}>
            <div class="flex flex-col gap-4">
              <p class="text-sm text-ink-muted">{t("catalog.publishHint")}</p>
              <div class="grid gap-4 md:grid-cols-2 md:items-end">
                <label class="block">
                  <span class="mb-1 block text-sm font-medium text-ink">
                    {t("catalog.publishMenu")}
                  </span>
                  <select
                    class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                    value={publishMenu()}
                    onChange={(event) => setPublishMenu(event.currentTarget.value)}
                  >
                    <option value="">{t("catalog.chooseMenu")}</option>
                    <For each={(menus() ?? []).filter((menu) => menu.status === "active")}>
                      {(menu) => <option value={menu.menu_id}>{menu.name}</option>}
                    </For>
                  </select>
                </label>
                <div>
                  <span class="mb-1 block text-sm font-medium text-ink">
                    {t("catalog.publishStore")}
                  </span>
                  <p class="min-h-touch rounded-token border border-line bg-surface-raised px-3 py-2 text-base text-ink">
                    {storeName() || t("catalog.publishStoreNone")}
                  </p>
                </div>
              </div>
              <div>
                <Button disabled={busy() || !storeId()} onClick={() => void doPublish()}>
                  {t("action.publish")}
                </Button>
              </div>
            </div>
          </Card>
        </div>
      </RequireContext>
    </div>
  );
}
