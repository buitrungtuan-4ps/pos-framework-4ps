// Kitchen stations & routing (ADR-0072, Track M2), on the F2 CRUD kit. The operator's place to define
// a store's kitchen stations — each with an optional backup (printer failover) and a catch-all default
// flag — and the rules that route a fired line to a station by item. All by name, no ULID typed.
// Stations and routing are per-store, so this screen needs a store chosen in the top bar; items come
// from the tenant's catalog. None of this is PII.
//
// Write affordances are gated on the operator holding console.floor.manage (owner/admin) — the server
// re-checks every route, so the gate here only hides what a role cannot do. Course-based routing is
// supported at the wire level; the console offers item routing (the item is picked from the catalog),
// so no course ULID is ever typed. Publishing here compiles the same floor plan the Floor screen does.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { CatalogItem, RoutingRule, Station } from "../api/types";
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

export function Stations() {
  const [stations, setStations] = createSignal<Station[] | null>(null);
  const [rules, setRules] = createSignal<RoutingRule[]>([]);
  const [items, setItems] = createSignal<CatalogItem[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // console.floor.manage → owner/admin (mirrors the backend role set; the server re-checks).
  const canManage = () => {
    const role = actingAdmin()?.role;
    return role === "owner" || role === "admin";
  };

  // Station editor (a Drawer). `stationDraftId` is "" for a new station, else the one being edited.
  const [stationOpen, setStationOpen] = createSignal(false);
  const [stationDraftId, setStationDraftId] = createSignal("");
  const [stationName, setStationName] = createSignal("");
  const [stationBackup, setStationBackup] = createSignal("");
  const [stationDefault, setStationDefault] = createSignal(false);
  const [pendingStationArchive, setPendingStationArchive] = createSignal<Station | null>(null);

  // New routing rule (station + item + sort) and the pending remove.
  const [ruleStation, setRuleStation] = createSignal("");
  const [ruleItem, setRuleItem] = createSignal("");
  const [ruleSort, setRuleSort] = createSignal("");
  const [pendingRuleRemove, setPendingRuleRemove] = createSignal<RoutingRule | null>(null);

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
      const [loadedStations, loadedRules, loadedItems] = await Promise.all([
        api.listStations(tenantId(), storeId()),
        api.listRoutingRules(tenantId(), storeId()),
        api.listItems(tenantId()),
      ]);
      setStations(loadedStations);
      setRules(loadedRules);
      setItems(loadedItems);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  const stationName_ = (id: string) =>
    stations()?.find((station) => station.station_id === id)?.name ?? id;
  const itemName = (id: string) =>
    items().find((item) => item.menu_item_id === id)?.name ?? id;
  const activeStations = () => stations()?.filter((station) => station.status === "active") ?? [];

  const openNewStation = () => {
    setStationDraftId("");
    setStationName("");
    setStationBackup("");
    setStationDefault(false);
    setStationOpen(true);
  };
  const openEditStation = (station: Station) => {
    setStationDraftId(station.station_id);
    setStationName(station.name);
    setStationBackup(station.backup_station_id ?? "");
    setStationDefault(station.is_default);
    setStationOpen(true);
  };

  const saveStation = async () => {
    const name = stationName().trim();
    if (!name) {
      setError(t("stations.nameRequired"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      if (stationDraftId()) {
        await api.updateStation(stationDraftId(), tenantId(), {
          name,
          backupStationId: stationBackup() || null,
          isDefault: stationDefault(),
          status: "active",
        });
        toast.ok(t("stations.stationUpdated"));
      } else {
        await api.createStation(tenantId(), storeId(), {
          name,
          backupStationId: stationBackup() || null,
          isDefault: stationDefault(),
        });
        toast.ok(t("stations.stationCreated"));
      }
      setStationOpen(false);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const setStationStatus = async (
    station: Station,
    status: "active" | "archived",
    doneMessage: string,
  ) => {
    setBusy(true);
    try {
      await api.updateStation(station.station_id, tenantId(), {
        name: station.name,
        backupStationId: station.backup_station_id,
        isDefault: station.is_default,
        status,
      });
      setPendingStationArchive(null);
      toast.ok(doneMessage);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const createRule = async () => {
    if (!ruleStation() || !ruleItem()) {
      setError(t("stations.needItemAndStation"));
      return;
    }
    const sort = Number(ruleSort().trim() || "0");
    if (!Number.isInteger(sort)) {
      setError(t("stations.sortInvalid"));
      return;
    }
    setError("");
    setBusy(true);
    try {
      await api.createRoutingRule(tenantId(), storeId(), {
        stationId: ruleStation(),
        menuItemId: ruleItem(),
        courseId: null,
        sort,
      });
      setRuleStation("");
      setRuleItem("");
      setRuleSort("");
      toast.ok(t("stations.ruleCreated"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const removeRule = async () => {
    const rule = pendingRuleRemove();
    if (!rule) {
      return;
    }
    setBusy(true);
    try {
      await api.removeRoutingRule(tenantId(), rule.rule_id);
      setPendingRuleRemove(null);
      toast.ok(t("stations.ruleRemoved"));
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
      toast.ok(t("stations.published"));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const ruleMatch = (rule: RoutingRule) => {
    if (rule.menu_item_id) {
      return itemName(rule.menu_item_id);
    }
    if (rule.course_id) {
      return t("stations.courseRule");
    }
    return t("common.unknown");
  };

  const stationColumns = (): Column<Station>[] => [
    {
      key: "name",
      header: t("stations.name"),
      sortValue: (row) => row.name,
      cell: (row) => <span>{row.name}</span>,
    },
    {
      key: "backup",
      header: t("stations.backup"),
      cell: (row) => (
        <span class="text-ink-muted">
          {row.backup_station_id ? stationName_(row.backup_station_id) : t("stations.noBackup")}
        </span>
      ),
    },
    {
      key: "default",
      header: t("stations.default"),
      cell: (row) => (
        <Show when={row.is_default} fallback={<span class="text-ink-muted">—</span>}>
          <StatusBadge tone="active" label={t("stations.defaultBadge")} />
        </Show>
      ),
    },
    {
      key: "status",
      header: t("stations.status"),
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
        <TechnicalDetails label={t("common.technicalDetails")}>{row.station_id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("stations.title")} description={t("stations.description")} />
      <RequireContext need="store">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Card
            title={t("stations.stations")}
            actions={
              <div class="flex gap-2">
                <Show when={canManage()}>
                  <Button disabled={busy()} onClick={openNewStation}>
                    {t("stations.addStation")}
                  </Button>
                </Show>
                <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <Show
              when={stations()}
              fallback={<p class="text-sm text-ink-muted">{t("stations.loadHint")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={stationColumns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={<EmptyState title={t("stations.stationsEmpty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Show when={canManage()}>
                      <div class="flex flex-wrap gap-2">
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => openEditStation(row)}
                        >
                          {t("stations.edit")}
                        </Button>
                        <Show
                          when={row.status === "archived"}
                          fallback={
                            <Button
                              variant="danger"
                              disabled={busy()}
                              onClick={() => setPendingStationArchive(row)}
                            >
                              {t("stations.archive")}
                            </Button>
                          }
                        >
                          <Button
                            variant="secondary"
                            disabled={busy()}
                            onClick={() =>
                              void setStationStatus(row, "active", t("stations.stationRestored"))
                            }
                          >
                            {t("stations.restore")}
                          </Button>
                        </Show>
                      </div>
                    </Show>
                  )}
                />
              )}
            </Show>
          </Card>

          <Card title={t("stations.routing")}>
            <div class="flex flex-col gap-4">
              <p class="text-sm text-ink-muted">{t("stations.routingHint")}</p>
              <Show
                when={activeStations().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("stations.needStationFirst")}</p>}
              >
                <Show when={canManage()}>
                  <div class="flex flex-wrap items-end gap-3">
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("stations.station")}
                      </span>
                      <select
                        class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        aria-label={t("stations.station")}
                        value={ruleStation()}
                        onChange={(event) => setRuleStation(event.currentTarget.value)}
                      >
                        <option value="">{t("stations.chooseStation")}</option>
                        <For each={activeStations()}>
                          {(station) => (
                            <option value={station.station_id}>{station.name}</option>
                          )}
                        </For>
                      </select>
                    </label>
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("stations.matchItem")}
                      </span>
                      <select
                        class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        aria-label={t("stations.matchItem")}
                        value={ruleItem()}
                        onChange={(event) => setRuleItem(event.currentTarget.value)}
                      >
                        <option value="">{t("stations.chooseItem")}</option>
                        <For each={items().filter((item) => item.status === "active")}>
                          {(item) => <option value={item.menu_item_id}>{item.name}</option>}
                        </For>
                      </select>
                    </label>
                    <label class="block w-24">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("stations.sort")}
                      </span>
                      <input
                        class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        type="number"
                        aria-label={t("stations.sort")}
                        value={ruleSort()}
                        onInput={(event) => setRuleSort(event.currentTarget.value)}
                      />
                    </label>
                    <Button disabled={busy()} onClick={() => void createRule()}>
                      {t("stations.addRule")}
                    </Button>
                  </div>
                </Show>
                <DataTable
                  columns={[
                    {
                      key: "station",
                      header: t("stations.station"),
                      cell: (row: RoutingRule) => <span>{stationName_(row.station_id)}</span>,
                    },
                    {
                      key: "match",
                      header: t("stations.match"),
                      cell: (row: RoutingRule) => (
                        <span class="text-ink-muted">{ruleMatch(row)}</span>
                      ),
                    },
                    {
                      key: "sort",
                      header: t("stations.sort"),
                      sortValue: (row: RoutingRule) => row.sort,
                      cell: (row: RoutingRule) => <span class="text-ink-muted">{row.sort}</span>,
                    },
                  ]}
                  rows={rules()}
                  empty={<EmptyState title={t("stations.routingEmpty")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Show when={canManage()}>
                      <Button
                        variant="danger"
                        disabled={busy()}
                        onClick={() => setPendingRuleRemove(row)}
                      >
                        {t("stations.removeRule")}
                      </Button>
                    </Show>
                  )}
                />
              </Show>
            </div>
          </Card>

          <Card title={t("stations.publish")}>
            <div class="flex flex-col gap-3">
              <p class="text-sm text-ink-muted">{t("stations.publishHint")}</p>
              <Show when={canManage()}>
                <div>
                  <Button disabled={busy()} onClick={() => void publish()}>
                    {t("stations.publishAction")}
                  </Button>
                </div>
              </Show>
            </div>
          </Card>
        </div>

        <Drawer
          open={stationOpen()}
          title={stationDraftId() ? t("stations.editStation") : t("stations.addStation")}
          closeLabel={t("action.close")}
          onClose={() => setStationOpen(false)}
          footer={
            <>
              <Button variant="secondary" onClick={() => setStationOpen(false)}>
                {t("action.cancel")}
              </Button>
              <Button disabled={busy()} onClick={() => void saveStation()}>
                {t("action.save")}
              </Button>
            </>
          }
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("stations.name")}
              value={stationName()}
              onInput={setStationName}
              placeholder={t("stations.namePlaceholder")}
            />
            <FormField label={t("stations.backup")}>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                aria-label={t("stations.backup")}
                value={stationBackup()}
                onChange={(event) => setStationBackup(event.currentTarget.value)}
              >
                <option value="">{t("stations.noBackup")}</option>
                <For
                  each={activeStations().filter(
                    (station) => station.station_id !== stationDraftId(),
                  )}
                >
                  {(station) => <option value={station.station_id}>{station.name}</option>}
                </For>
              </select>
            </FormField>
            <label class="flex items-start gap-2 text-sm text-ink">
              <input
                type="checkbox"
                class="mt-1"
                aria-label={t("stations.default")}
                checked={stationDefault()}
                onChange={(event) => setStationDefault(event.currentTarget.checked)}
              />
              <span>
                <span class="font-medium">{t("stations.default")}</span>
                <span class="block text-xs text-ink-muted">{t("stations.defaultHint")}</span>
              </span>
            </label>
          </div>
        </Drawer>

        <ConfirmDialog
          open={pendingStationArchive() !== null}
          title={t("stations.archiveStationTitle")}
          message={t("stations.archiveStationMessage")}
          confirmLabel={t("stations.archive")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const station = pendingStationArchive();
            if (station) {
              void setStationStatus(station, "archived", t("stations.stationArchived"));
            }
          }}
          onCancel={() => setPendingStationArchive(null)}
        />

        <ConfirmDialog
          open={pendingRuleRemove() !== null}
          title={t("stations.removeRuleTitle")}
          message={t("stations.removeRuleMessage")}
          confirmLabel={t("stations.removeRule")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void removeRule()}
          onCancel={() => setPendingRuleRemove(null)}
        />
      </RequireContext>
    </div>
  );
}
