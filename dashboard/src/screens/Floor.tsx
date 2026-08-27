// Floor plan (ADR-0072, Track M2), on the F2 CRUD kit. The operator's place to lay out a store's
// areas and tables, publish the plan to the store, and print a QR sheet — all by name, no ULID typed.
// A floor is per-store, so this screen needs a store chosen in the top bar; the areas/tables lists
// carry both tenant and store. None of this is PII.
//
// Write affordances are gated on the operator holding console.floor.manage (owner/admin) — the server
// re-checks every route, so the gate here only hides what a role cannot do. The visual pointer-drag
// editor is deliberately deferred to F3; placement is set here by numeric grid column/row (or left
// unplaced), which is enough for the plan the edge and the QR sheet consume.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { Area, FloorTable, TableQrToken } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  FormField,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

export function Floor() {
  const [areas, setAreas] = createSignal<Area[] | null>(null);
  const [tables, setTables] = createSignal<FloorTable[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // console.floor.manage → owner/admin (mirrors the backend role set; the server re-checks).
  const canManage = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };

  // New area + inline rename (mirrors Stores).
  const [newAreaName, setNewAreaName] = createSignal("");
  const [editingArea, setEditingArea] = createSignal("");
  const [areaDraft, setAreaDraft] = createSignal("");
  const [pendingAreaArchive, setPendingAreaArchive] = createSignal<Area | null>(null);

  // Table editor (a Drawer). `tableDraftId` is "" for a new table, else the table being edited.
  const [tableOpen, setTableOpen] = createSignal(false);
  const [tableDraftId, setTableDraftId] = createSignal("");
  const [tableArea, setTableArea] = createSignal("");
  const [tableLabel, setTableLabel] = createSignal("");
  const [tableSeats, setTableSeats] = createSignal("");
  const [tableColumn, setTableColumn] = createSignal("");
  const [tableRow, setTableRow] = createSignal("");
  const [pendingTableArchive, setPendingTableArchive] = createSignal<FloorTable | null>(null);

  // QR sheet (loaded on demand). `null` = not loaded; the route is absent when no table-token secret
  // is configured on the cloud, which surfaces as a 404 → a gentle "unavailable" note, not an error.
  const [qr, setQr] = createSignal<TableQrToken[] | null>(null);
  const [qrUnavailable, setQrUnavailable] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    if (!storeId()) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      const [loadedAreas, loadedTables] = await Promise.all([
        api.listAreas(tenantId(), storeId()),
        api.listTables(tenantId(), storeId()),
      ]);
      setAreas(loadedAreas);
      setTables(loadedTables);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  const areaName = (id: string) =>
    areas()?.find((area) => area.area_id === id)?.name ?? id;
  const activeAreas = () => areas()?.filter((area) => area.status === "active") ?? [];

  const createArea = async () => {
    const name = newAreaName().trim();
    if (!name) {
      setError(t("floor.areaNameRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.createArea(tenantId(), storeId(), name);
      setNewAreaName("");
      toast.ok(t("floor.areaCreated"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const saveAreaName = async (area: Area) => {
    const name = areaDraft().trim();
    if (!name) {
      setError(t("floor.areaNameRequired"));
      return;
    }
    setBusy(true);
    try {
      await api.updateArea(area.area_id, tenantId(), { name, status: area.status });
      setEditingArea("");
      setAreaDraft("");
      toast.ok(t("floor.areaRenamed"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setAreaStatus = async (
    area: Area,
    status: "active" | "archived",
    doneMessage: string,
  ) => {
    setBusy(true);
    try {
      await api.updateArea(area.area_id, tenantId(), { name: area.name, status });
      setPendingAreaArchive(null);
      toast.ok(doneMessage);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const openNewTable = () => {
    setTableDraftId("");
    setTableArea(activeAreas()[0]?.area_id ?? "");
    setTableLabel("");
    setTableSeats("");
    setTableColumn("");
    setTableRow("");
    setTableOpen(true);
  };
  const openEditTable = (table: FloorTable) => {
    setTableDraftId(table.table_id);
    setTableArea(table.area_id);
    setTableLabel(table.label);
    setTableSeats(String(table.seats));
    setTableColumn(table.position ? String(table.position.column) : "");
    setTableRow(table.position ? String(table.position.row) : "");
    setTableOpen(true);
  };

  const saveTable = async () => {
    if (!tableArea()) {
      setError(t("floor.chooseArea"));
      return;
    }
    const label = tableLabel().trim();
    if (!label) {
      setError(t("floor.labelRequired"));
      return;
    }
    const seats = Number(tableSeats().trim() || "0");
    if (!Number.isInteger(seats) || seats < 0) {
      setError(t("floor.seatsInvalid"));
      return;
    }
    // A grid slot needs both column and row, or neither (an unplaced table).
    const rawColumn = tableColumn().trim();
    const rawRow = tableRow().trim();
    let gridColumn: number | null = null;
    let gridRow: number | null = null;
    if (rawColumn || rawRow) {
      const column = Number(rawColumn);
      const row = Number(rawRow);
      if (!Number.isInteger(column) || column < 0 || !Number.isInteger(row) || row < 0) {
        setError(t("floor.gridInvalid"));
        return;
      }
      gridColumn = column;
      gridRow = row;
    }
    setError("");
    setBusy(true);
    try {
      if (tableDraftId()) {
        await api.updateTable(tableDraftId(), tenantId(), {
          areaId: tableArea(),
          name: label,
          seats,
          gridColumn,
          gridRow,
          status: "active",
        });
        toast.ok(t("floor.tableUpdated"));
      } else {
        await api.createTable(tenantId(), storeId(), {
          areaId: tableArea(),
          name: label,
          seats,
          gridColumn,
          gridRow,
        });
        toast.ok(t("floor.tableCreated"));
      }
      setTableOpen(false);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setTableStatus = async (
    table: FloorTable,
    status: "active" | "archived",
    doneMessage: string,
  ) => {
    setBusy(true);
    try {
      await api.updateTable(table.table_id, tenantId(), {
        areaId: table.area_id,
        name: table.label,
        seats: table.seats,
        gridColumn: table.position?.column ?? null,
        gridRow: table.position?.row ?? null,
        status,
      });
      setPendingTableArchive(null);
      toast.ok(doneMessage);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publish = async () => {
    setBusy(true);
    try {
      await api.publishFloor(tenantId(), storeId());
      toast.ok(t("floor.published"));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const loadQr = async () => {
    setError("");
    setQrUnavailable(false);
    setBusy(true);
    try {
      const sheet = await api.tableQrTokens(tenantId(), storeId());
      setQr([...sheet.tokens]);
      toast.ok(t("floor.qrLoaded"));
    } catch (caught) {
      // The route is absent (404) when the cloud has no table-token secret configured — a
      // configuration state, not an error the operator can fix here.
      if (caught instanceof ApiError && caught.status === 404) {
        setQr([]);
        setQrUnavailable(true);
      } else {
        fail(caught);
      }
    } finally {
      setBusy(false);
    }
  };

  const areaColumns = (): Column<Area>[] => [
    {
      key: "name",
      header: t("floor.areaName"),
      sortValue: (row) => row.name,
      cell: (row) => (
        <Show when={editingArea() === row.area_id} fallback={<span>{row.name}</span>}>
          <div class="flex flex-wrap items-center gap-2">
            <input
              class="min-h-touch w-44 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
              aria-label={t("floor.areaName")}
              value={areaDraft()}
              onInput={(event) => setAreaDraft(event.currentTarget.value)}
            />
            <Button disabled={busy()} onClick={() => void saveAreaName(row)}>
              {t("action.save")}
            </Button>
            <Button
              variant="secondary"
              onClick={() => {
                setEditingArea("");
                setAreaDraft("");
              }}
            >
              {t("action.cancel")}
            </Button>
          </div>
        </Show>
      ),
    },
    {
      key: "status",
      header: t("floor.status"),
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
        <TechnicalDetails label={t("common.technicalDetails")}>{row.area_id}</TechnicalDetails>
      ),
    },
  ];

  const tableColumns = (): Column<FloorTable>[] => [
    {
      key: "label",
      header: t("floor.label"),
      sortValue: (row) => row.label,
      cell: (row) => <span>{row.label}</span>,
    },
    {
      key: "area",
      header: t("floor.area"),
      sortValue: (row) => areaName(row.area_id),
      cell: (row) => <span class="text-ink-muted">{areaName(row.area_id)}</span>,
    },
    {
      key: "seats",
      header: t("floor.seats"),
      sortValue: (row) => row.seats,
      cell: (row) => <span class="text-ink-muted">{row.seats}</span>,
    },
    {
      key: "position",
      header: t("floor.position"),
      cell: (row) => (
        <span class="text-ink-muted">
          {row.position
            ? t("floor.gridAt", { column: row.position.column, row: row.position.row })
            : t("floor.unplaced")}
        </span>
      ),
    },
    {
      key: "status",
      header: t("floor.status"),
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
        <TechnicalDetails label={t("common.technicalDetails")}>{row.table_id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("floor.title")} description={t("floor.description")} />
      <RequireContext need="store">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Card
            title={t("floor.areas")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={areas()}
              fallback={<p class="text-sm text-ink-muted">{t("floor.loadHint")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={areaColumns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={<EmptyState title={t("floor.areasEmpty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Show when={canManage()}>
                      <div class="flex flex-wrap gap-2">
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => {
                            setEditingArea(row.area_id);
                            setAreaDraft(row.name);
                          }}
                        >
                          {t("floor.rename")}
                        </Button>
                        <Show
                          when={row.status === "archived"}
                          fallback={
                            <Button
                              variant="danger"
                              disabled={busy()}
                              onClick={() => setPendingAreaArchive(row)}
                            >
                              {t("floor.archive")}
                            </Button>
                          }
                        >
                          <Button
                            variant="secondary"
                            disabled={busy()}
                            onClick={() => void setAreaStatus(row, "active", t("floor.areaRestored"))}
                          >
                            {t("floor.restore")}
                          </Button>
                        </Show>
                      </div>
                    </Show>
                  )}
                />
              )}
            </Show>
          </Card>

          <Show when={canManage()}>
            <Card title={t("floor.addArea")}>
              <div class="flex flex-col gap-4">
                <TextField
                  label={t("floor.areaName")}
                  value={newAreaName()}
                  onInput={setNewAreaName}
                  placeholder={t("floor.areaNamePlaceholder")}
                />
                <Button disabled={busy()} onClick={() => void createArea()}>
                  {t("action.create")}
                </Button>
              </div>
            </Card>
          </Show>

          <Card
            title={t("floor.tables")}
            actions={
              <Show when={canManage()}>
                <Button
                  disabled={busy() || activeAreas().length === 0}
                  onClick={openNewTable}
                >
                  {t("floor.addTable")}
                </Button>
              </Show>
            }
          >
            <Show
              when={activeAreas().length > 0}
              fallback={<p class="text-sm text-ink-muted">{t("floor.needAreaFirst")}</p>}
            >
              <DataTable
                columns={tableColumns()}
                rows={tables()}
                searchText={(row) => `${row.label} ${areaName(row.area_id)}`}
                pageSize={12}
                empty={<EmptyState title={t("floor.tablesEmpty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <Show when={canManage()}>
                    <div class="flex flex-wrap gap-2">
                      <Button
                        variant="secondary"
                        disabled={busy()}
                        onClick={() => openEditTable(row)}
                      >
                        {t("floor.edit")}
                      </Button>
                      <Show
                        when={row.status === "archived"}
                        fallback={
                          <Button
                            variant="danger"
                            disabled={busy()}
                            onClick={() => setPendingTableArchive(row)}
                          >
                            {t("floor.archive")}
                          </Button>
                        }
                      >
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => void setTableStatus(row, "active", t("floor.tableRestored"))}
                        >
                          {t("floor.restore")}
                        </Button>
                      </Show>
                    </div>
                  </Show>
                )}
              />
            </Show>
          </Card>

          <Card title={t("floor.publish")}>
            <div class="flex flex-col gap-3">
              <p class="text-sm text-ink-muted">{t("floor.publishHint")}</p>
              <Show when={canManage()}>
                <div>
                  <Button disabled={busy()} onClick={() => void publish()}>
                    {t("floor.publishAction")}
                  </Button>
                </div>
              </Show>
            </div>
          </Card>

          <Card
            title={t("floor.qr")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void loadQr()}>
                {t("floor.qrLoad")}
              </Button>
            }
          >
            <div class="flex flex-col gap-3">
              <p class="text-sm text-ink-muted">{t("floor.qrHint")}</p>
              <Show when={qrUnavailable()}>
                <Banner tone="danger" message={t("floor.qrUnavailable")} />
              </Show>
              <Show when={!qrUnavailable() && qr()}>
                {(tokens) => (
                  <Show
                    when={tokens().length > 0}
                    fallback={<EmptyState title={t("floor.qrEmpty")} />}
                  >
                    <DataTable
                      columns={[
                        {
                          key: "table",
                          header: t("floor.qrTable"),
                          cell: (row: TableQrToken) => <span>{row.label}</span>,
                        },
                        {
                          key: "token",
                          header: t("floor.qrToken"),
                          class: "font-mono",
                          cell: (row: TableQrToken) => (
                            <span class="break-all text-xs text-ink-muted">{row.token}</span>
                          ),
                        },
                      ]}
                      rows={tokens()}
                      empty={<EmptyState title={t("floor.qrEmpty")} />}
                    />
                  </Show>
                )}
              </Show>
            </div>
          </Card>
        </div>

        <Drawer
          open={tableOpen()}
          title={tableDraftId() ? t("floor.editTable") : t("floor.addTable")}
          closeLabel={t("action.close")}
          onClose={() => setTableOpen(false)}
          footer={
            <>
              <Button variant="secondary" onClick={() => setTableOpen(false)}>
                {t("action.cancel")}
              </Button>
              <Button disabled={busy()} onClick={() => void saveTable()}>
                {t("action.save")}
              </Button>
            </>
          }
        >
          <div class="flex flex-col gap-4">
            <FormField label={t("floor.area")}>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                aria-label={t("floor.area")}
                value={tableArea()}
                onChange={(event) => setTableArea(event.currentTarget.value)}
              >
                <option value="">{t("floor.chooseArea")}</option>
                <For each={activeAreas()}>
                  {(area) => <option value={area.area_id}>{area.name}</option>}
                </For>
              </select>
            </FormField>
            <TextField
              label={t("floor.label")}
              value={tableLabel()}
              onInput={setTableLabel}
              placeholder={t("floor.labelPlaceholder")}
            />
            <TextField
              label={t("floor.seats")}
              type="number"
              value={tableSeats()}
              onInput={setTableSeats}
              placeholder={t("floor.seatsPlaceholder")}
            />
            <p class="text-sm text-ink-muted">{t("floor.gridHint")}</p>
            <div class="grid grid-cols-2 gap-3">
              <TextField
                label={t("floor.gridColumn")}
                type="number"
                value={tableColumn()}
                onInput={setTableColumn}
              />
              <TextField
                label={t("floor.gridRow")}
                type="number"
                value={tableRow()}
                onInput={setTableRow}
              />
            </div>
          </div>
        </Drawer>

        <ConfirmDialog
          open={pendingAreaArchive() !== null}
          title={t("floor.archiveAreaTitle")}
          message={t("floor.archiveAreaMessage")}
          confirmLabel={t("floor.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const area = pendingAreaArchive();
            if (area) {
              void setAreaStatus(area, "archived", t("floor.areaArchived"));
            }
          }}
          onCancel={() => setPendingAreaArchive(null)}
        />

        <ConfirmDialog
          open={pendingTableArchive() !== null}
          title={t("floor.archiveTableTitle")}
          message={t("floor.archiveTableMessage")}
          confirmLabel={t("floor.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const table = pendingTableArchive();
            if (table) {
              void setTableStatus(table, "archived", t("floor.tableArchived"));
            }
          }}
          onCancel={() => setPendingTableArchive(null)}
        />
      </RequireContext>
    </div>
  );
}
