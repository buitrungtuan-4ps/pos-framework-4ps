// The Items sub-screen (ADR-0082, Track F3): the product master on the F2 CRUD kit. Behaviour is
// preserved from the monolith — create (name + required tax class + optional taxonomy), rename with
// per-locale names (ADR-0074), the inline image widget (ADR-0075), archive/restore, and the
// owner/admin CSV export — but rendered as a searchable `DataTable` with the ULID behind a
// `TechnicalDetails` disclosure, a `Drawer` for create and for edit, and the shared `StatusCell`.
// Tax class, category and sub-category are set at create and preserved on every edit, exactly as the
// monolith did (a rename or a status flip re-sends the item's existing taxonomy untouched).

import { createEffect, createSignal, For, Show } from "solid-js";
import { useSearchParams } from "@solidjs/router";

import { api } from "../../api/client";
import type { CatalogItem, ItemImportReport, ItemSort } from "../../api/types";
import { LOCALES, localeName, t } from "../../i18n";
import { createAdminResource, failureOf } from "../../lib/resource";
import { actingAdmin, tenantId } from "../../state/session";
import { Banner, Button, Card, FileButton, SelectField, TextField } from "../../components/ui";
import {
  type Column,
  DataTable,
  Drawer,
  EmptyState,
  Modal,
  TechnicalDetails,
  Toolbar,
} from "../../components/kit";
import { toast } from "../../components/Toast";
import { ImagePicker } from "../../components/ImagePicker";
import { cleanTranslations, errorMessage, isStale, StatusCell } from "./shared";

/**
 * How many items one page of the table carries.
 *
 * The read is paged server-side (ADR-0098), so the search box and the sortable headers ask the
 * server rather than filtering the page: an item master runs to thousands for a chain, and a box
 * that searched only the visible twelve would fail to find most of them.
 */
const PAGE_SIZE = 25;

export function CatalogItems() {
  // The window the operator is looking at. A view parameter, not load state: the read closure
  // below reads it, so moving the pager is `setOffset` then `refetch` (the Media precedent).
  const [offset, setOffset] = createSignal(0);
  // The applied search, and the text being typed. They differ so a keystroke does not fire a read.
  const [search, setSearch] = createSignal("");
  const [searchDraft, setSearchDraft] = createSignal("");
  const [sort, setSort] = createSignal<ItemSort>("newest");
  const [descending, setDescending] = createSignal(false);
  // Write-in-flight only; the read's own refusal is `failureOf` below.
  const [busy, setBusy] = createSignal(false);

  // One read: the item page, and the three taxonomies the table needs to name what is in it. The
  // taxonomy reads stay unpaged — they fill the drawer's selects and resolve an id to a label, so
  // each needs its whole (small) set, and a page that arrived without them renders ULIDs.
  // The tenant whose catalogue the view parameters below describe. A page-four offset and a search
  // for one organisation's product mean nothing in another, so the read resets them when the tenant
  // changes — here rather than in a second context gate, which would race the resource's own and
  // cost two reads per switch.
  let describing = "";
  const catalogue = createAdminResource(
    async (tenant) => {
      if (tenant !== describing) {
        describing = tenant;
        setOffset(0);
        setSearch("");
        setSearchDraft("");
      }
      const [page, taxClasses, categories, subcategories] = await Promise.all([
        api.listItemsPage(
          tenant,
          { limit: PAGE_SIZE, offset: offset() },
          {
            q: search().trim() || undefined,
            sort: sort(),
            order: descending() ? "desc" : "asc",
          },
        ),
        api.listTaxClasses(tenant),
        api.listItemCategories(tenant),
        api.listItemSubcategories(tenant),
      ]);
      return { page, taxClasses, categories, subcategories };
    },
    { scope: "tenant" },
  );

  const items = () => catalogue.value()?.page.items ?? null;
  const total = () => catalogue.value()?.page.total ?? 0;
  const taxClasses = () => catalogue.value()?.taxClasses ?? [];
  const categories = () => catalogue.value()?.categories ?? [];
  const subcategories = () => catalogue.value()?.subcategories ?? [];

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

  /**
   * Move the pager and read that window, stepping back off a window that no longer exists.
   *
   * A page that comes back empty from somewhere other than the start means the matching set shrank
   * under the pager — a narrowed search, or an item just archived off the last page. Walking back
   * beats showing an empty table over a non-zero count, which reads as "your catalogue is gone".
   * The walk is bounded: each step halves the offset towards zero, and zero is allowed to be empty.
   */
  const show = async (from: number): Promise<void> => {
    let at = Math.max(0, from);
    for (;;) {
      setOffset(at);
      await catalogue.refetch();
      const page = catalogue.value()?.page;
      if (at === 0 || page === undefined || page.items.length > 0) {
        return;
      }
      at = Math.max(0, at - PAGE_SIZE);
    }
  };

  /** Applies the typed search and returns to the first page — a page-four offset means nothing now. */
  const applySearch = () => {
    setSearch(searchDraft());
    void show(0);
  };

  // A name handed over by the command palette (roadmap-v3 **F16**). The console has no route to one
  // item, so the palette sends the catalogue the words and the catalogue searches for them — which
  // is also the more useful landing, because an operator looking for "Phở" usually wants the four
  // things called that rather than one row.
  //
  // Reactive on the parameter, not on the box: it fires when the palette navigates here and does not
  // fire again, so an operator who then clears the search box is not fought by the URL.
  const [params] = useSearchParams<{ q?: string }>();
  createEffect(() => {
    const handed = (params.q ?? "").trim();
    if (handed) {
      setSearchDraft(handed);
      setSearch(handed);
      void show(0);
    }
  });

  /** Re-reads the set in a new order, from its first page. */
  const applySort = (field: string, wantsDescending: boolean) => {
    setSort(field as ItemSort);
    setDescending(wantsDescending);
    void show(0);
  };

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
      await show(offset());
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
      }, item.etag);
      await show(offset());
      return true;
    } catch (caught) {
      toast.error(errorMessage(caught));
      // A stale copy is recovered by reloading, so the reader sees what actually changed.
      if (isStale(caught)) {
        await show(offset());
      }
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

  // CSV import, dry-run first (ADR-0075, F15). The file is held between the two steps so the
  // confirm re-sends the exact bytes the review classified — re-picking the file to apply would
  // let the operator confirm one file and send another.
  const [importReport, setImportReport] = createSignal<ItemImportReport | null>(null);
  const [importFile, setImportFile] = createSignal<File | null>(null);

  const closeImport = () => {
    setImportReport(null);
    setImportFile(null);
  };

  // Step 1: classify every row and show the verdicts. Nothing is written.
  const reviewImport = async (file: File) => {
    setBusy(true);
    try {
      const report = await api.dryRunItemsCsv(tenantId(), file);
      setImportFile(file);
      setImportReport(report);
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  // Step 2: the operator confirms, and the same bytes are applied. The report comes back a second
  // time because a row can still be refused at the write — the item changed while the review was
  // open — and the operator needs to see which.
  const applyImport = async () => {
    const file = importFile();
    if (!file) {
      return;
    }
    setBusy(true);
    try {
      const report = await api.applyItemsCsv(tenantId(), file);
      if (report.reject_count > 0) {
        toast.error(
          t("catalog.importPartial", {
            created: report.create_count,
            updated: report.update_count,
            rejected: report.reject_count,
          }),
        );
        setImportReport(report);
      } else {
        toast.ok(
          t("catalog.importApplied", {
            created: report.create_count,
            updated: report.update_count,
          }),
        );
        closeImport();
      }
      // An import can create rows, so the window the operator is on may have moved under them.
      await show(offset());
    } catch (caught) {
      toast.error(errorMessage(caught));
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<CatalogItem>[] => [
    {
      key: "name",
      header: t("catalog.name"),
      sortField: "name",
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
      // No sort: the value shown is a tax class's *name*, resolved from another table. The server
      // orders `catalog_items`, and sorting the page by a label would order twenty-five rows as if
      // they were the master. Sorting by a joined label is a bigger question than this slice.
      cell: (row) => taxClassName(row.tax_class_id),
    },
    {
      key: "category",
      header: t("catalog.category"),
      // Not sortable, for the reason the tax class column gives.
      cell: (row) => categoryName(row.item_category_id),
    },
    {
      key: "status",
      header: t("catalog.status"),
      sortField: "status",
      cell: (row) => <StatusCell status={row.status} />,
    },
  ];

  return (
    <div class="flex flex-col gap-6">
      {/* A refusal is its own state, not an empty table (D5). */}
      <Show when={failureOf(catalogue)}>
        {(message) => <Banner tone="danger" message={message()} />}
      </Show>

      <Card
        title={t("catalog.items")}
        actions={
          <div class="flex flex-wrap gap-2">
            <Show when={canManageMedia()}>
              <Button variant="secondary" disabled={busy()} onClick={() => void exportItems()}>
                {t("catalog.exportCsv")}
              </Button>
              <FileButton
                label={t("catalog.importCsv")}
                accept=".csv,text/csv"
                variant="secondary"
                disabled={busy()}
                onPick={(file) => void reviewImport(file)}
              />
            </Show>
            <Button disabled={busy()} onClick={openCreate}>
              {t("catalog.createItem")}
            </Button>
          </div>
        }
      >
        {/*
          The search box is the screen's own, not the DataTable's: the table's box filters the rows
          it was handed, and the rows it is handed are one page. This one asks the server, so it
          searches the whole master — including each item's per-locale names (ADR-0074), which is
          what an operator typing Vietnamese needs.
        */}
        <Toolbar
          trailing={
            <>
              <Button variant="secondary" disabled={busy()} onClick={applySearch}>
                {t("action.search")}
              </Button>
              <Show when={search()}>
                <Button
                  variant="secondary"
                  disabled={busy()}
                  onClick={() => {
                    setSearchDraft("");
                    setSearch("");
                    void show(0);
                  }}
                >
                  {t("action.clear")}
                </Button>
              </Show>
            </>
          }
        >
          <TextField
            label={t("catalog.searchItems")}
            value={searchDraft()}
            onInput={setSearchDraft}
            placeholder={t("catalog.searchItemsHint")}
          />
        </Toolbar>

        <Show
          when={items()}
          fallback={<p class="text-sm text-ink-muted">{t("catalog.loadHint")}</p>}
        >
          {(loaded) => (
            <DataTable
              columns={columns()}
              rows={loaded()}
              pageSize={PAGE_SIZE}
              serverTotal={total()}
              onPage={(next) => void show(next)}
              onSort={applySort}
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
          <SelectField
            label={t("catalog.taxClass")}
            value={newTaxClass()}
            options={activeTaxClasses().map((row) => ({
              value: row.tax_class_id,
              label: row.name,
            }))}
            onChange={setNewTaxClass}
            placeholder={t("catalog.chooseTaxClass")}
            // The hint is only true when the list is empty, and it is the whole reason an operator
            // would be stuck here: nothing to pick and nothing saying why.
            hint={activeTaxClasses().length === 0 ? t("catalog.taxClassEmpty") : undefined}
          />
          <SelectField
            label={t("catalog.category")}
            value={newCategory()}
            options={activeCategories().map((row) => ({
              value: row.item_category_id,
              label: row.name,
            }))}
            onChange={(value) => {
              setNewCategory(value);
              // A subcategory belongs to one category, so the old pick cannot survive the change.
              setNewSubcategory("");
            }}
            placeholder={t("catalog.noCategory")}
          />
          <SelectField
            label={t("catalog.subcategory")}
            value={newSubcategory()}
            options={activeSubcategories().map((row) => ({
              value: row.item_subcategory_id,
              label: row.name,
            }))}
            onChange={setNewSubcategory}
            placeholder={t("catalog.noSubcategory")}
            disabled={!newCategory()}
          />
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
                    <TextField
                      label={localeName(code)}
                      value={draftTranslations()[code] ?? ""}
                      onInput={(value) =>
                        setDraftTranslations({ ...draftTranslations(), [code]: value })
                      }
                    />
                  )}
                </For>
              </div>
            </div>
          </div>
        </Show>
      </Drawer>

      {/*
        The review dialog: what the file would do, before it does it. The rejected rows are listed
        with their reasons rather than counted, because a spreadsheet is fixed row by row and a
        number tells the operator nothing about which one to look at.
      */}
      <Modal
        open={importReport() !== null}
        title={t("catalog.importReview")}
        closeLabel={t("action.close")}
        onClose={closeImport}
        footer={
          <>
            <Button variant="secondary" onClick={closeImport}>
              {t("action.cancel")}
            </Button>
            <Button
              disabled={
                busy() ||
                (importReport()?.create_count ?? 0) + (importReport()?.update_count ?? 0) === 0
              }
              onClick={() => void applyImport()}
            >
              {t("catalog.importApply")}
            </Button>
          </>
        }
      >
        <Show when={importReport()}>
          {(report) => (
            <div class="flex flex-col gap-3 text-sm text-ink">
              <p>
                {t("catalog.importSummary", {
                  created: report().create_count,
                  updated: report().update_count,
                  rejected: report().reject_count,
                })}
              </p>
              <Show when={report().reject_count > 0}>
                <div class="flex flex-col gap-1">
                  <p class="font-medium">{t("catalog.importRejected")}</p>
                  <ul class="max-h-40 overflow-y-auto text-xs text-ink-muted">
                    <For each={report().rows.filter((row) => row.action === "reject")}>
                      {(row) => (
                        <li>
                          {row.key || t("catalog.importUnnamedRow")} —{" "}
                          {"reason" in row ? row.reason : ""}
                        </li>
                      )}
                    </For>
                  </ul>
                </div>
              </Show>
            </div>
          )}
        </Show>
      </Modal>
    </div>
  );
}
