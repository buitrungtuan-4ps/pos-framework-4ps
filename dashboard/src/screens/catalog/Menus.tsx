// The Menus sub-screen (ADR-0082, Track F3): menus, their authoring sections, the per-channel priced
// placements, and publish-to-store — on the F2 CRUD kit. This is the priced heart of the catalog, so
// it also lands the two F3 additions the ADR calls for: prices are edited through the new currency-
// aware `MoneyField` (integer minor units, locale-grouped, currency chosen from the country list, no
// more free-text currency), and a **bulk price editor** sets one channel's price across a section's
// placements at once. Everything else is behaviour-preserving from the monolith: menu inheritance,
// section sort, availability, and the publish path that compiles the menu onto the store's config.

import { createSignal, For, Show } from "solid-js";

import { api } from "../../api/client";
import type {
  CatalogItem,
  ChannelPrice,
  Country,
  Menu,
  MenuPlacement,
  MenuSection,
  SalesChannel,
} from "../../api/types";
import { SALES_CHANNELS } from "../../api/types";
import { t } from "../../i18n";
import { formatMoney } from "../../lib/format";
import { onScopedContext } from "../../lib/scoped";
import { storeId, storeName, tenantId } from "../../state/session";
import { Banner, Button, Card, MoneyField, TextField } from "../../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  FormField,
  TechnicalDetails,
} from "../../components/kit";
import { toast } from "../../components/Toast";
import { CHANNEL_LABEL, emptyPriceSheet, errorMessage, StatusCell } from "./shared";

export function CatalogMenus() {
  const [menus, setMenus] = createSignal<Menu[] | null>(null);
  const [items, setItems] = createSignal<CatalogItem[]>([]);
  const [countries, setCountries] = createSignal<Country[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [selectedMenu, setSelectedMenu] = createSignal("");
  const [sections, setSections] = createSignal<MenuSection[]>([]);
  const [placements, setPlacements] = createSignal<MenuPlacement[] | null>(null);

  // Menu create/edit drawers.
  const [creatingMenu, setCreatingMenu] = createSignal(false);
  const [newMenuName, setNewMenuName] = createSignal("");
  const [newMenuParent, setNewMenuParent] = createSignal("");
  const [editingMenu, setEditingMenu] = createSignal<Menu | null>(null);
  const [draftMenuName, setDraftMenuName] = createSignal("");
  const [draftMenuParent, setDraftMenuParent] = createSignal("");

  // Section create/edit drawers.
  const [creatingSection, setCreatingSection] = createSignal(false);
  const [newSectionName, setNewSectionName] = createSignal("");
  const [newSectionSort, setNewSectionSort] = createSignal("0");
  const [editingSection, setEditingSection] = createSignal<MenuSection | null>(null);
  const [draftSectionName, setDraftSectionName] = createSignal("");
  const [draftSectionSort, setDraftSectionSort] = createSignal("0");

  // Placement editor drawer.
  const [placementOpen, setPlacementOpen] = createSignal(false);
  const [placementEditing, setPlacementEditing] = createSignal(false);
  const [placementItem, setPlacementItem] = createSignal("");
  const [placementCurrency, setPlacementCurrency] = createSignal("VND");
  const [placementSection, setPlacementSection] = createSignal("");
  const [placementAvailable, setPlacementAvailable] = createSignal(true);
  const [priceSheet, setPriceSheet] = createSignal(emptyPriceSheet());
  const [pendingRemove, setPendingRemove] = createSignal<MenuPlacement | null>(null);

  // Bulk price editor (a Drawer over the current menu's placements).
  const [bulkOpen, setBulkOpen] = createSignal(false);
  const [bulkSection, setBulkSection] = createSignal("");
  const [bulkChannel, setBulkChannel] = createSignal<SalesChannel>("SALES_CHANNEL_DINE_IN");
  const [bulkCurrency, setBulkCurrency] = createSignal("VND");
  const [bulkAmount, setBulkAmount] = createSignal<number | null>(null);

  // Publish.
  const [publishMenu, setPublishMenu] = createSignal("");

  const menuName = (id: string) => menus()?.find((menu) => menu.menu_id === id)?.name ?? id;
  const itemName = (id: string) =>
    items().find((item) => item.menu_item_id === id)?.name ?? id;
  const sectionName = (id: string | null) =>
    id ? (sections().find((row) => row.menu_section_id === id)?.name ?? id) : "—";

  // The currency codes the operator can pick, from the country registry (deduped, sorted); VND is the
  // v1 default and the fallback while the list loads.
  const currencyOptions = () => {
    const codes = new Set(countries().map((country) => country.currency_code));
    codes.add("VND");
    return [...codes].sort((a, b) => a.localeCompare(b));
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [loadedMenus, loadedItems, loadedCountries] = await Promise.all([
        api.listMenus(tenantId()),
        api.listItems(tenantId()),
        api.listCountries(),
      ]);
      setMenus(loadedMenus);
      setItems(loadedItems);
      setCountries(loadedCountries);
      if (selectedMenu()) {
        await loadMenuDetail(selectedMenu());
      }
    } catch (caught) {
      setError(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const loadMenuDetail = async (menuId: string) => {
    const [loadedPlacements, loadedSections] = await Promise.all([
      api.listPlacements(tenantId(), menuId),
      api.listMenuSections(tenantId(), menuId),
    ]);
    setPlacements(loadedPlacements);
    setSections(loadedSections);
  };

  const openMenuDetail = async (menuId: string) => {
    setSelectedMenu(menuId);
    resetPlacementEditor();
    setBusy(true);
    try {
      await loadMenuDetail(menuId);
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // --- menus ---

  const openCreateMenu = () => {
    setNewMenuName("");
    setNewMenuParent("");
    setCreatingMenu(true);
  };

  const createMenu = async () => {
    const name = newMenuName().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createMenu(tenantId(), name, newMenuParent() || undefined);
      toast.ok(t("catalog.menuCreated"));
      setCreatingMenu(false);
      await load();
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applyMenu = async (
    menu: Menu,
    fields: { name?: string; parentMenuId?: string | null; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? menu.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateMenu(menu.menu_id, tenantId(), {
        name,
        parentMenuId: fields.parentMenuId === undefined ? menu.parent_menu_id : fields.parentMenuId,
        status: fields.status ?? menu.status,
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

  const openEditMenu = (menu: Menu) => {
    setEditingMenu(menu);
    setDraftMenuName(menu.name);
    setDraftMenuParent(menu.parent_menu_id ?? "");
  };

  const saveMenu = async () => {
    const menu = editingMenu();
    if (!menu) {
      return;
    }
    const ok = await applyMenu(menu, {
      name: draftMenuName(),
      parentMenuId: draftMenuParent() || null,
    });
    if (ok) {
      toast.ok(t("catalog.menuSaved"));
      setEditingMenu(null);
    }
  };

  const toggleMenu = async (menu: Menu) => {
    const archiving = menu.status !== "archived";
    const ok = await applyMenu(menu, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.menuArchived") : t("catalog.menuRestored"));
    }
  };

  // --- sections ---

  const openCreateSection = () => {
    setNewSectionName("");
    setNewSectionSort("0");
    setCreatingSection(true);
  };

  const createSection = async () => {
    const name = newSectionName().trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return;
    }
    const sort = Number(newSectionSort());
    if (!Number.isInteger(sort)) {
      toast.error(t("catalog.sortInvalid"));
      return;
    }
    setBusy(true);
    try {
      await api.createMenuSection(tenantId(), selectedMenu(), name, sort);
      toast.ok(t("catalog.sectionCreated"));
      setCreatingSection(false);
      await loadMenuDetail(selectedMenu());
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const applySection = async (
    section: MenuSection,
    fields: { name?: string; sort?: number; status?: "active" | "archived" },
  ): Promise<boolean> => {
    const name = (fields.name ?? section.name).trim();
    if (!name) {
      toast.error(t("catalog.nameRequired"));
      return false;
    }
    setBusy(true);
    try {
      await api.updateMenuSection(tenantId(), selectedMenu(), section.menu_section_id, {
        name,
        sort: fields.sort ?? section.sort,
        status: fields.status ?? section.status,
      });
      await loadMenuDetail(selectedMenu());
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const openEditSection = (section: MenuSection) => {
    setEditingSection(section);
    setDraftSectionName(section.name);
    setDraftSectionSort(String(section.sort));
  };

  const saveSection = async () => {
    const section = editingSection();
    if (!section) {
      return;
    }
    const sort = Number(draftSectionSort());
    if (!Number.isInteger(sort)) {
      toast.error(t("catalog.sortInvalid"));
      return;
    }
    const ok = await applySection(section, { name: draftSectionName(), sort });
    if (ok) {
      toast.ok(t("catalog.sectionSaved"));
      setEditingSection(null);
    }
  };

  const toggleSection = async (section: MenuSection) => {
    const archiving = section.status !== "archived";
    const ok = await applySection(section, { status: archiving ? "archived" : "active" });
    if (ok) {
      toast.ok(archiving ? t("catalog.sectionArchived") : t("catalog.sectionRestored"));
    }
  };

  // --- placements ---

  const resetPlacementEditor = () => {
    setPlacementOpen(false);
    setPlacementEditing(false);
    setPlacementItem("");
    setPlacementCurrency("VND");
    setPlacementSection("");
    setPlacementAvailable(true);
    setPriceSheet(emptyPriceSheet());
  };

  const openAddPlacement = () => {
    resetPlacementEditor();
    setPlacementOpen(true);
  };

  const openEditPlacement = (placement: MenuPlacement) => {
    const sheet = emptyPriceSheet();
    let currency = "VND";
    for (const price of placement.prices) {
      if (price.sales_channel) {
        sheet[price.sales_channel] = price.unit_price.amount_minor;
        currency = price.unit_price.currency_code;
      }
    }
    setPlacementItem(placement.menu_item_id);
    setPlacementSection(placement.menu_section_id ?? "");
    setPlacementCurrency(currency);
    setPlacementAvailable(placement.available);
    setPriceSheet(sheet);
    setPlacementEditing(true);
    setPlacementOpen(true);
  };

  const setChannelAmount = (channel: SalesChannel, amount: number | null) =>
    setPriceSheet({ ...priceSheet(), [channel]: amount });

  const pricesFromSheet = (
    sheet: Record<SalesChannel, number | null>,
    currency: string,
  ): ChannelPrice[] => {
    const prices: ChannelPrice[] = [];
    for (const channel of SALES_CHANNELS) {
      const amount = sheet[channel];
      if (amount !== null) {
        prices.push({
          sales_channel: channel,
          unit_price: { currency_code: currency, amount_minor: amount },
        });
      }
    }
    return prices;
  };

  const savePlacement = async () => {
    const item = placementItem();
    if (!item) {
      toast.error(t("catalog.itemRequired"));
      return;
    }
    const currency = placementCurrency().trim() || "VND";
    setBusy(true);
    try {
      await api.setPlacement(
        tenantId(),
        selectedMenu(),
        item,
        pricesFromSheet(priceSheet(), currency),
        placementAvailable(),
        placementSection() || null,
      );
      toast.ok(t("catalog.placementSaved"));
      resetPlacementEditor();
      await loadMenuDetail(selectedMenu());
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const removePlacement = async () => {
    const placement = pendingRemove();
    if (!placement) {
      return;
    }
    setBusy(true);
    try {
      await api.deletePlacement(tenantId(), selectedMenu(), placement.menu_item_id);
      toast.ok(t("catalog.placementRemoved"));
      setPendingRemove(null);
      await loadMenuDetail(selectedMenu());
    } catch (caught) {
      toast.error(errorMessage(caught));
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

  // --- bulk price editing (ADR-0082) ---

  const openBulk = () => {
    setBulkSection("");
    setBulkChannel("SALES_CHANNEL_DINE_IN");
    setBulkCurrency("VND");
    setBulkAmount(null);
    setBulkOpen(true);
  };

  // Sets one channel's price across every placement in the chosen section (or all placements when no
  // section is chosen), preserving each placement's other channel prices, section, and availability.
  // A clear (empty amount) removes that channel's price from each in scope. N× the audited setPlacement.
  const applyBulk = async () => {
    const rows = placements() ?? [];
    const scope = bulkSection()
      ? rows.filter((row) => (row.menu_section_id ?? "") === bulkSection())
      : rows;
    if (scope.length === 0) {
      toast.error(t("catalog.bulkNoTargets"));
      return;
    }
    const channel = bulkChannel();
    const currency = bulkCurrency().trim() || "VND";
    const amount = bulkAmount();
    setBusy(true);
    try {
      for (const placement of scope) {
        // Start from the placement's existing prices, then set/clear the target channel.
        const others = placement.prices.filter(
          (price) => price.sales_channel && price.sales_channel !== channel,
        );
        const next: ChannelPrice[] =
          amount === null
            ? others
            : [
                ...others,
                { sales_channel: channel, unit_price: { currency_code: currency, amount_minor: amount } },
              ];
        await api.setPlacement(
          tenantId(),
          selectedMenu(),
          placement.menu_item_id,
          next,
          placement.available,
          placement.menu_section_id,
        );
      }
      toast.ok(t("catalog.bulkApplied", { count: scope.length }));
      setBulkOpen(false);
      await loadMenuDetail(selectedMenu());
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // --- publish ---

  const doPublish = async () => {
    if (!publishMenu()) {
      toast.error(t("catalog.menuRequired"));
      return;
    }
    if (!storeId()) {
      toast.error(t("context.storeRequired"));
      return;
    }
    setBusy(true);
    try {
      const result = await api.publishMenu(tenantId(), storeId(), publishMenu());
      toast.ok(t("catalog.published", { version: result.config_version_id }));
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const activeItems = () => items().filter((item) => item.status === "active");
  const activeSections = () => sections().filter((row) => row.status === "active");

  const menuColumns = (): Column<Menu>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => (
        <div class="flex flex-col gap-1">
          <span>{row.name}</span>
          <TechnicalDetails label={t("common.technicalDetails")}>
            <div>{row.menu_id}</div>
          </TechnicalDetails>
        </div>
      ),
    },
    {
      key: "parent",
      header: t("catalog.parent"),
      cell: (row) => (row.parent_menu_id ? menuName(row.parent_menu_id) : t("catalog.noParent")),
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  const sectionColumns = (): Column<MenuSection>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortValue: (row) => row.name,
      cell: (row) => row.name,
    },
    {
      key: "sort",
      header: t("catalog.sort"),
      sortValue: (row) => row.sort,
      cell: (row) => row.sort,
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortValue: (row) => row.status,
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  const placementColumns = (): Column<MenuPlacement>[] => [
    {
      key: "item",
      header: t("catalog.item"),
      sortValue: (row) => itemName(row.menu_item_id),
      cell: (row) => itemName(row.menu_item_id),
    },
    {
      key: "section",
      header: t("catalog.section"),
      cell: (row) => sectionName(row.menu_section_id),
    },
    {
      key: "prices",
      header: t("catalog.prices"),
      cell: (row) => priceSummary(row),
    },
    {
      key: "available",
      header: t("catalog.available"),
      cell: (row) => (row.available ? t("catalog.availableYes") : t("catalog.availableNo")),
    },
  ];

  return (
    <div class="flex flex-col gap-6">
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

      <Card
        title={t("catalog.menus")}
        actions={
          <div class="flex flex-wrap gap-2">
            <Button disabled={busy()} onClick={openCreateMenu}>
              {t("catalog.createMenu")}
            </Button>
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          </div>
        }
      >
        <Show
          when={menus()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={menuColumns()}
              rows={loaded()}
              searchText={(row) => row.name}
              pageSize={12}
              empty={<EmptyState title={t("catalog.menusEmpty")} />}
              actionsHeader={t("common.actions")}
              actions={(row) => (
                <div class="flex flex-wrap gap-2">
                  <Button disabled={busy()} onClick={() => void openMenuDetail(row.menu_id)}>
                    {t("catalog.openPlacements")}
                  </Button>
                  <Button variant="secondary" disabled={busy()} onClick={() => openEditMenu(row)}>
                    {t("action.edit")}
                  </Button>
                  <Button
                    variant={row.status === "archived" ? "secondary" : "danger"}
                    disabled={busy()}
                    onClick={() => void toggleMenu(row)}
                  >
                    {row.status === "archived" ? t("catalog.restore") : t("catalog.archive")}
                  </Button>
                </div>
              )}
            />
          )}
        </Show>
      </Card>

      <Show when={selectedMenu()}>
        <Card
          title={t("catalog.sectionsFor", { menu: menuName(selectedMenu()) })}
          actions={
            <Button disabled={busy()} onClick={openCreateSection}>
              {t("action.create")}
            </Button>
          }
        >
          <p class="mb-3 text-sm text-ink-muted">{t("catalog.sectionsHint")}</p>
          <DataTable
            columns={sectionColumns()}
            rows={sections()}
            searchText={(row) => row.name}
            empty={<EmptyState title={t("catalog.sectionsEmpty")} />}
            actionsHeader={t("common.actions")}
            actions={(row) => (
              <div class="flex flex-wrap gap-2">
                <Button variant="secondary" disabled={busy()} onClick={() => openEditSection(row)}>
                  {t("action.edit")}
                </Button>
                <Button
                  variant={row.status === "archived" ? "secondary" : "danger"}
                  disabled={busy()}
                  onClick={() => void toggleSection(row)}
                >
                  {row.status === "archived" ? t("catalog.restore") : t("catalog.archive")}
                </Button>
              </div>
            )}
          />
        </Card>

        <Card
          title={t("catalog.placementsFor", { menu: menuName(selectedMenu()) })}
          actions={
            <div class="flex flex-wrap gap-2">
              <Button disabled={busy()} onClick={openAddPlacement}>
                {t("catalog.addPlacement")}
              </Button>
              <Button variant="secondary" disabled={busy() || !placements()} onClick={openBulk}>
                {t("catalog.bulkPrice")}
              </Button>
            </div>
          }
        >
          <Show
            when={placements()}
            fallback={<p class="text-sm text-ink-muted">{t("catalog.chooseMenuHint")}</p>}
          >
            {(loaded) => (
              <DataTable
                columns={placementColumns()}
                rows={loaded()}
                searchText={(row) => itemName(row.menu_item_id)}
                empty={<EmptyState title={t("catalog.placementsEmpty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex flex-wrap gap-2">
                    <Button variant="secondary" disabled={busy()} onClick={() => openEditPlacement(row)}>
                      {t("action.edit")}
                    </Button>
                    <Button variant="danger" disabled={busy()} onClick={() => setPendingRemove(row)}>
                      {t("catalog.remove")}
                    </Button>
                  </div>
                )}
              />
            )}
          </Show>
        </Card>
      </Show>

      <Card title={t("catalog.publish")}>
        <div class="flex flex-col gap-4">
          <p class="text-sm text-ink-muted">{t("catalog.publishHint")}</p>
          <div class="grid gap-4 md:grid-cols-2 md:items-end">
            <FormField label={t("catalog.publishMenu")}>
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
            </FormField>
            <div>
              <span class="mb-1 block text-sm font-medium text-ink">{t("catalog.publishStore")}</span>
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

      {/* Menu create / edit */}
      <Drawer
        open={creatingMenu()}
        title={t("catalog.createMenu")}
        closeLabel={t("action.close")}
        onClose={() => setCreatingMenu(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreatingMenu(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createMenu()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <TextField
            label={t("catalog.name")}
            value={newMenuName()}
            onInput={setNewMenuName}
            placeholder={t("catalog.menuNamePlaceholder")}
          />
          <FormField label={t("catalog.parent")}>
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
          </FormField>
        </div>
      </Drawer>

      <Drawer
        open={editingMenu() !== null}
        title={editingMenu()?.name ?? t("action.edit")}
        closeLabel={t("action.close")}
        onClose={() => setEditingMenu(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditingMenu(null)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void saveMenu()}>
              {t("action.save")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <TextField label={t("catalog.name")} value={draftMenuName()} onInput={setDraftMenuName} />
          <FormField label={t("catalog.parent")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={draftMenuParent()}
              onChange={(event) => setDraftMenuParent(event.currentTarget.value)}
            >
              <option value="">{t("catalog.noParent")}</option>
              <For each={(menus() ?? []).filter((menu) => menu.menu_id !== editingMenu()?.menu_id)}>
                {(menu) => <option value={menu.menu_id}>{menu.name}</option>}
              </For>
            </select>
          </FormField>
        </div>
      </Drawer>

      {/* Section create / edit */}
      <Drawer
        open={creatingSection()}
        title={t("catalog.sectionName")}
        closeLabel={t("action.close")}
        onClose={() => setCreatingSection(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setCreatingSection(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void createSection()}>
              {t("action.create")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <TextField
            label={t("catalog.sectionName")}
            value={newSectionName()}
            onInput={setNewSectionName}
            placeholder={t("catalog.sectionNamePlaceholder")}
          />
          <TextField label={t("catalog.sort")} value={newSectionSort()} onInput={setNewSectionSort} />
        </div>
      </Drawer>

      <Drawer
        open={editingSection() !== null}
        title={editingSection()?.name ?? t("action.edit")}
        closeLabel={t("action.close")}
        onClose={() => setEditingSection(null)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setEditingSection(null)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void saveSection()}>
              {t("action.save")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <TextField
            label={t("catalog.sectionName")}
            value={draftSectionName()}
            onInput={setDraftSectionName}
          />
          <TextField
            label={t("catalog.sort")}
            value={draftSectionSort()}
            onInput={setDraftSectionSort}
          />
        </div>
      </Drawer>

      {/* Placement editor */}
      <Drawer
        open={placementOpen()}
        title={placementEditing() ? t("catalog.editPlacement") : t("catalog.addPlacement")}
        closeLabel={t("action.close")}
        onClose={resetPlacementEditor}
        footer={
          <>
            <Button variant="secondary" onClick={resetPlacementEditor}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void savePlacement()}>
              {t("catalog.savePlacement")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <FormField label={t("catalog.item")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink disabled:opacity-50"
              value={placementItem()}
              disabled={placementEditing()}
              onChange={(event) => setPlacementItem(event.currentTarget.value)}
            >
              <option value="">{t("catalog.chooseItem")}</option>
              <For each={activeItems()}>
                {(item) => <option value={item.menu_item_id}>{item.name}</option>}
              </For>
            </select>
          </FormField>
          <FormField label={t("catalog.currency")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={placementCurrency()}
              onChange={(event) => setPlacementCurrency(event.currentTarget.value)}
            >
              <For each={currencyOptions()}>{(code) => <option value={code}>{code}</option>}</For>
            </select>
          </FormField>
          <FormField label={t("catalog.section")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={placementSection()}
              onChange={(event) => setPlacementSection(event.currentTarget.value)}
            >
              <option value="">{t("catalog.sectionNone")}</option>
              <For each={activeSections()}>
                {(row) => <option value={row.menu_section_id}>{row.name}</option>}
              </For>
            </select>
          </FormField>
          <div>
            <p class="mb-1 text-sm font-medium text-ink">{t("catalog.prices")}</p>
            <p class="mb-3 text-xs text-ink-muted">{t("catalog.pricesHint")}</p>
            <div class="grid gap-3">
              <For each={SALES_CHANNELS}>
                {(channel) => (
                  <MoneyField
                    label={t(CHANNEL_LABEL[channel])}
                    currencyCode={placementCurrency()}
                    value={priceSheet()[channel]}
                    onChange={(amount) => setChannelAmount(channel, amount)}
                  />
                )}
              </For>
            </div>
          </div>
          <label class="flex items-center gap-2 text-sm text-ink">
            <input
              type="checkbox"
              class="size-5"
              checked={placementAvailable()}
              onChange={(event) => setPlacementAvailable(event.currentTarget.checked)}
            />
            {t("catalog.availableLabel")}
          </label>
        </div>
      </Drawer>

      {/* Bulk price editor */}
      <Drawer
        open={bulkOpen()}
        title={t("catalog.bulkPrice")}
        closeLabel={t("action.close")}
        onClose={() => setBulkOpen(false)}
        footer={
          <>
            <Button variant="secondary" onClick={() => setBulkOpen(false)}>
              {t("action.cancel")}
            </Button>
            <Button disabled={busy()} onClick={() => void applyBulk()}>
              {t("catalog.bulkApply")}
            </Button>
          </>
        }
      >
        <div class="flex flex-col gap-4">
          <p class="text-sm text-ink-muted">{t("catalog.bulkPriceHint")}</p>
          <FormField label={t("catalog.section")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={bulkSection()}
              onChange={(event) => setBulkSection(event.currentTarget.value)}
            >
              <option value="">{t("catalog.bulkAllSections")}</option>
              <For each={activeSections()}>
                {(row) => <option value={row.menu_section_id}>{row.name}</option>}
              </For>
            </select>
          </FormField>
          <FormField label={t("catalog.bulkChannel")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={bulkChannel()}
              onChange={(event) => setBulkChannel(event.currentTarget.value as SalesChannel)}
            >
              <For each={SALES_CHANNELS}>
                {(channel) => <option value={channel}>{t(CHANNEL_LABEL[channel])}</option>}
              </For>
            </select>
          </FormField>
          <FormField label={t("catalog.currency")}>
            <select
              class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
              value={bulkCurrency()}
              onChange={(event) => setBulkCurrency(event.currentTarget.value)}
            >
              <For each={currencyOptions()}>{(code) => <option value={code}>{code}</option>}</For>
            </select>
          </FormField>
          <MoneyField
            label={t("catalog.amount")}
            currencyCode={bulkCurrency()}
            value={bulkAmount()}
            onChange={setBulkAmount}
          />
          <p class="text-xs text-ink-muted">{t("catalog.bulkClearHint")}</p>
        </div>
      </Drawer>

      <ConfirmDialog
        open={pendingRemove() !== null}
        title={t("catalog.removePlacementTitle")}
        message={t("catalog.removePlacementMessage")}
        confirmLabel={t("catalog.remove")}
        cancelLabel={t("action.cancel")}
        closeLabel={t("action.close")}
        danger
        busy={busy()}
        onConfirm={() => void removePlacement()}
        onCancel={() => setPendingRemove(null)}
      />
    </div>
  );
}
