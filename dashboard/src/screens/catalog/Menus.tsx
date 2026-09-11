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
  ETag,
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
import {
  Banner,
  Button,
  Card,
  ComboboxField,
  CheckboxField,
  MoneyField,
  SelectField,
  TextField,
} from "../../components/ui";
import {
  type Column,
  CLIENT_PAGE_SIZE,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  TechnicalDetails,
} from "../../components/kit";
import { toast } from "../../components/Toast";
import { CHANNEL_LABEL, emptyPriceSheet, errorMessage, isStale, StatusCell } from "./shared";

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
  // Non-null while editing a placement already on the menu, carrying the version it was read at:
  // the update is conditional on that version (ADR-0095). `null` means the drawer is adding one.
  const [placementEditing, setPlacementEditing] = createSignal<{ etag: ETag } | null>(null);
  const [placementItem, setPlacementItem] = createSignal("");
  const [placementCurrency, setPlacementCurrency] = createSignal("VND");
  const [placementSection, setPlacementSection] = createSignal("");
  const [placementAvailable, setPlacementAvailable] = createSignal(true);
  const [priceSheet, setPriceSheet] = createSignal(emptyPriceSheet());
  const [pendingRemove, setPendingRemove] = createSignal<MenuPlacement | null>(null);

  // Bulk price editor (a Drawer over the current menu's placements).
  const [bulkOpen, setBulkOpen] = createSignal(false);
  /**
   * What the last bulk apply did to each row it could not change.
   *
   * `null` until an apply has run. Empty after one that changed everything in scope — which is not
   * the same as `null`, and the difference is the whole point: an operator who has just been told
   * "48 of 50" needs the other two named.
   */
  const [bulkRefused, setBulkRefused] = createSignal<
    { readonly item: string; readonly message: string }[] | null
  >(null);
  const [bulkAppliedCount, setBulkAppliedCount] = createSignal(0);
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
      // `listItems` and not `listItemsPage`, deliberately, and this is the one place in the console
      // where that is a considered choice rather than an oversight.
      //
      // Two things read this list, and they want opposite shapes. The placement picker wants a
      // window — but it is a `ComboboxField` now, which builds its options only when it is opened
      // and filters them in the browser against every locale's name, so a held list costs nothing
      // per render and answers a keystroke with no round-trip. The placements *table* wants the
      // opposite: it renders `menu_item_id` for every row and has to turn each one into a name, and
      // a placement can point at any item in the master, so a window would show ULIDs for whatever
      // fell outside it.
      //
      // The real fix is for `list_placements` to return the item's name with the placement, which
      // removes the coupling entirely and lets the picker be served. That is a read-model change in
      // `pos-cloud` and it is not in this slice; until then the honest trade is one bounded fetch on
      // screen entry over a table that cannot name its own rows.
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
      }, menu.etag);
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
      }, section.etag);
      await loadMenuDetail(selectedMenu());
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      // A stale copy is recovered by reloading, so the reader sees what actually changed.
      if (isStale(caught)) {
        await loadMenuDetail(selectedMenu());
      }
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
    setPlacementEditing(null);
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
    setPlacementEditing({ etag: placement.etag });
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
    const prices = pricesFromSheet(priceSheet(), currency);
    setBusy(true);
    try {
      const editing = placementEditing();
      if (editing) {
        await api.updatePlacement(
          tenantId(),
          selectedMenu(),
          item,
          editing.etag,
          prices,
          placementAvailable(),
          placementSection() || null,
        );
      } else {
        // A placement's identity is the `(menu, item)` pair chosen here, so adding an item already
        // on the menu is refused rather than repricing it — and since the per-channel prices are the
        // price-change journal (ADR-0069), that overwrite left no `before` behind (ADR-0095).
        await api.createPlacement(
          tenantId(),
          selectedMenu(),
          item,
          prices,
          placementAvailable(),
          placementSection() || null,
        );
      }
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
    setBulkRefused(null);
    setBulkAppliedCount(0);
    setBulkSection("");
    setBulkChannel("SALES_CHANNEL_DINE_IN");
    setBulkCurrency("VND");
    setBulkAmount(null);
    setBulkOpen(true);
  };

  // Sets one channel's price across every placement in the chosen section (or all placements when no
  // section is chosen), preserving each placement's other channel prices, section, and availability.
  // A clear (empty amount) removes that channel's price from each in scope. N× the audited update.
  //
  // # Why this does not stop at the first refusal
  //
  // There is no transaction here and there was never going to be one: each placement is its own
  // conditional write (ADR-0095), and a bulk change is N of them. What the first version got wrong
  // was not the absence of atomicity but the absence of a *report* — it `await`ed inside a `for`,
  // so one refusal aborted the rest and the operator was shown a single error with no way to learn
  // how many of fifty prices had already changed.
  //
  // Worse, the state it left was a trap. The rows that succeeded had new versions the screen was
  // still holding the old ones for, so re-running the bulk refused every row that had worked and
  // applied every row that had not — the exact inverse of what the operator was trying to do, and
  // it looked like the tool was broken rather than stale.
  //
  // So: attempt every row, collect the refusals with the item they belong to, and reload the menu
  // afterwards **whatever happened**, so the versions on screen are the versions on the server and
  // a retry means what it says.
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
    setBulkRefused(null);
    const refused: { item: string; message: string }[] = [];
    let applied = 0;
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
      try {
        // Each placement in scope was just read, so its reprice is conditional on the version it
        // carried: a bulk change against a row someone else has edited is refused rather than
        // overwriting their price (ADR-0095).
        await api.updatePlacement(
          tenantId(),
          selectedMenu(),
          placement.menu_item_id,
          placement.etag,
          next,
          placement.available,
          placement.menu_section_id,
        );
        applied += 1;
      } catch (caught) {
        refused.push({ item: itemName(placement.menu_item_id), message: errorMessage(caught) });
      }
    }
    setBulkAppliedCount(applied);
    setBulkRefused(refused);
    // Unconditional, and load-bearing: the rows that succeeded now hold versions this screen does
    // not, and a retry against the stale ones is the trap described above.
    await loadMenuDetail(selectedMenu());
    setBusy(false);
    if (refused.length === 0) {
      toast.ok(t("catalog.bulkApplied", { count: applied }));
      setBulkOpen(false);
      return;
    }
    // The drawer stays open, because the report is the outcome. Its numbers are the two an
    // operator has to act on, and a toast that scrolls away is not where they belong.
    toast.error(t("catalog.bulkPartial", { applied, refused: refused.length }));
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
            pageSize={CLIENT_PAGE_SIZE}
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
                pageSize={CLIENT_PAGE_SIZE}
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
            <SelectField
              label={t("catalog.publishMenu")}
              value={publishMenu()}
              options={(menus() ?? [])
                .filter((menu) => menu.status === "active")
                .map((menu) => ({ value: menu.menu_id, label: menu.name }))}
              onChange={setPublishMenu}
              placeholder={t("catalog.chooseMenu")}
            />
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
          <SelectField
            label={t("catalog.parent")}
            value={newMenuParent()}
            options={(menus() ?? []).map((menu) => ({ value: menu.menu_id, label: menu.name }))}
            onChange={setNewMenuParent}
            placeholder={t("catalog.noParent")}
          />
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
          <SelectField
            label={t("catalog.parent")}
            value={draftMenuParent()}
            // A menu cannot be its own parent, so the one being edited is not on offer.
            options={(menus() ?? [])
              .filter((menu) => menu.menu_id !== editingMenu()?.menu_id)
              .map((menu) => ({ value: menu.menu_id, label: menu.name }))}
            onChange={setDraftMenuParent}
            placeholder={t("catalog.noParent")}
          />
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
          <ComboboxField
            label={t("catalog.item")}
            value={placementItem()}
            options={activeItems().map((item) => ({
              value: item.menu_item_id,
              label: item.name,
              keywords: Object.values(item.name_translations),
            }))}
            onChange={setPlacementItem}
            placeholder={t("catalog.chooseItem")}
            searchLabel={t("catalog.searchItems")}
            emptyLabel={t("picker.noMatch")}
            // Which item a placement is for is fixed once it exists: changing it would be a
            // different placement, so an edit offers everything else and not this.
            disabled={placementEditing() !== null}
          />
          <SelectField
            label={t("catalog.currency")}
            value={placementCurrency()}
            options={currencyOptions().map((code) => ({ value: code, label: code }))}
            onChange={setPlacementCurrency}
          />
          <SelectField
            label={t("catalog.section")}
            value={placementSection()}
            options={activeSections().map((row) => ({
              value: row.menu_section_id,
              label: row.name,
            }))}
            onChange={setPlacementSection}
            placeholder={t("catalog.sectionNone")}
          />
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
          <CheckboxField
            label={t("catalog.availableLabel")}
            checked={placementAvailable()}
            onChange={setPlacementAvailable}
          />
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
          <Show when={bulkRefused()}>
            {(rows) => (
              <Show
                when={rows().length > 0}
                fallback={
                  <Banner tone="ok" message={t("catalog.bulkApplied", { count: bulkAppliedCount() })} />
                }
              >
                <div class="rounded-token border border-danger p-3">
                  <p class="text-sm font-medium text-ink">
                    {t("catalog.bulkPartial", {
                      applied: bulkAppliedCount(),
                      refused: rows().length,
                    })}
                  </p>
                  <p class="mt-1 text-sm text-ink-muted">{t("catalog.bulkPartialHint")}</p>
                  <ul class="mt-2 flex flex-col gap-1">
                    <For each={rows()}>
                      {(row) => (
                        <li class="text-sm text-ink">
                          <span class="font-medium">{row.item}</span>
                          <span class="text-ink-muted"> — {row.message}</span>
                        </li>
                      )}
                    </For>
                  </ul>
                </div>
              </Show>
            )}
          </Show>
          <p class="text-sm text-ink-muted">{t("catalog.bulkPriceHint")}</p>
          <SelectField
            label={t("catalog.section")}
            value={bulkSection()}
            options={activeSections().map((row) => ({
              value: row.menu_section_id,
              label: row.name,
            }))}
            onChange={setBulkSection}
            placeholder={t("catalog.bulkAllSections")}
          />
          <SelectField
            label={t("catalog.bulkChannel")}
            value={bulkChannel()}
            options={SALES_CHANNELS.map((channel) => ({
              value: channel,
              label: t(CHANNEL_LABEL[channel]),
            }))}
            onChange={(value) => setBulkChannel(value as SalesChannel)}
          />
          <SelectField
            label={t("catalog.currency")}
            value={bulkCurrency()}
            options={currencyOptions().map((code) => ({ value: code, label: code }))}
            onChange={setBulkCurrency}
          />
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
