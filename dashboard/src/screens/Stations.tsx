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

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { CatalogItem, RoutingRule, Station } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, storeId, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  ComboboxField,
  CheckboxField,
  PageHeader,
  SelectField,
  StatusBadge,
  TextField,
} from "../components/ui";
import {
  type Column,
  CLIENT_PAGE_SIZE,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage, isStale } from "../lib/errors";

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
  // The version the drawer opened on (ADR-0094). Empty for a new station, which has none yet.
  const [stationDraftEtag, setStationDraftEtag] = createSignal("");
  const [stationName, setStationName] = createSignal("");
  const [stationBackup, setStationBackup] = createSignal("");
  const [stationDefault, setStationDefault] = createSignal(false);
  const [pendingStationArchive, setPendingStationArchive] = createSignal<Station | null>(null);

  // New routing rule (station + item + sort) and the pending remove.
  const [ruleStation, setRuleStation] = createSignal("");
  const [ruleItem, setRuleItem] = createSignal("");
  const [ruleSort, setRuleSort] = createSignal("");
  const [pendingRuleRemove, setPendingRuleRemove] = createSignal<RoutingRule | null>(null);

  // A `412` means somebody else saved this station while the form was open (ADR-0094). The screen
  // reloads rather than offering a retry: retrying would re-apply the overwrite the refusal exists
  // to prevent, and the operator needs to see what actually changed before deciding again.
  const fail = async (caught: unknown) => {
    if (isStale(caught)) {
      const message = t("stations.stale");
      setError(message);
      toast.error(message);
      await load();
      return;
    }
    const message = apiMessage(caught);
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
      await fail(caught);
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
    setStationDraftEtag("");
    setStationName("");
    setStationBackup("");
    setStationDefault(false);
    setStationOpen(true);
  };
  const openEditStation = (station: Station) => {
    setStationDraftId(station.station_id);
    setStationDraftEtag(station.etag);
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
        await api.updateStation(
          stationDraftId(),
          tenantId(),
          {
            name,
            backupStationId: stationBackup() || null,
            isDefault: stationDefault(),
            status: "active",
          },
          stationDraftEtag(),
        );
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
      await fail(caught);
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
      await api.updateStation(
        station.station_id,
        tenantId(),
        {
          name: station.name,
          backupStationId: station.backup_station_id,
          isDefault: station.is_default,
          status,
        },
        station.etag,
      );
      setPendingStationArchive(null);
      toast.ok(doneMessage);
      await load();
    } catch (caught) {
      await fail(caught);
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
      await fail(caught);
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
      await fail(caught);
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
      await fail(caught);
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
                    <SelectField
                      label={t("stations.station")}
                      value={ruleStation()}
                      options={activeStations().map((station) => ({
                        value: station.station_id,
                        label: station.name,
                      }))}
                      onChange={setRuleStation}
                      placeholder={t("stations.chooseStation")}
                    />
                    <ComboboxField
                      label={t("stations.matchItem")}
                      value={ruleItem()}
                      options={items()
                        .filter((item) => item.status === "active")
                        .map((item) => ({ value: item.menu_item_id, label: item.name }))}
                      onChange={setRuleItem}
                      placeholder={t("stations.chooseItem")}
                      searchLabel={t("catalog.searchItems")}
                      emptyLabel={t("picker.noMatch")}
                    />
                    <div class="w-24">
                      <TextField
                        label={t("stations.sort")}
                        type="number"
                        value={ruleSort()}
                        onInput={setRuleSort}
                      />
                    </div>
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
                  pageSize={CLIENT_PAGE_SIZE}
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
            <SelectField
              label={t("stations.backup")}
              value={stationBackup()}
              // A station cannot be its own backup, so the one being edited is not on offer.
              options={activeStations()
                .filter((station) => station.station_id !== stationDraftId())
                .map((station) => ({ value: station.station_id, label: station.name }))}
              onChange={setStationBackup}
              placeholder={t("stations.noBackup")}
            />
            <CheckboxField
              label={t("stations.default")}
              checked={stationDefault()}
              onChange={setStationDefault}
              caption={
                <span>
                  <span class="text-sm font-medium text-ink">{t("stations.default")}</span>
                  <span class="block text-xs text-ink-muted">{t("stations.defaultHint")}</span>
                </span>
              }
            />
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
