// Fleet liveness + background-task health (ADR-0068, Track O1 slice 5). One operational glance: which
// stores are online, whether each is on the current config, how deep its order-relay backlog is — and,
// above the fleet, whether the cloud's own background loops are alive and keeping up. Read-only and
// tenant-scoped (the fleet) plus a fleet-wide health strip; it polls so the answer stays current
// without a manual refresh.

import { createSignal, For, onCleanup, onMount, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { FleetStore, TaskHealthEntry, TaskHealthReport } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { formatCount, formatRelativeAge } from "../lib/format";
import { contextReady, onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import {
  type Column,
  DataTable,
  Drawer,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/** How often the view re-reads the fleet and health, so "online" and "last seen" stay current. */
const POLL_MS = 15_000;

/** Friendly labels for the background loops the health endpoint reports; an unmapped task name (a
 *  loop added later) shows verbatim rather than a blank. */
const TASK_LABELS: Record<string, MessageKey> = {
  rollup_projector: "fleet.task.rollupProjector",
  retention: "fleet.task.retention",
  webhook_dispatcher: "fleet.task.webhookDispatcher",
};

function taskLabel(task: string): string {
  const key = TASK_LABELS[task];
  return key ? t(key) : task;
}

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

export function Fleet() {
  const [stores, setStores] = createSignal<FleetStore[] | null>(null);
  const [health, setHealth] = createSignal<TaskHealthReport | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [selected, setSelected] = createSignal<FleetStore | null>(null);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      // The two reads are independent (fleet is tenant-scoped, health is fleet-wide); fetch together.
      const [loadedStores, loadedHealth] = await Promise.all([
        api.listFleet(tenantId()),
        api.taskHealth(),
      ]);
      setStores(loadedStores);
      setHealth(loadedHealth);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes (never with an empty context, F0).
  onScopedContext("tenant", () => void load());

  // Poll while the screen is open, but only once a tenant is chosen — an empty context never fetches.
  onMount(() => {
    const handle = setInterval(() => {
      if (contextReady("tenant") && !busy()) {
        void load();
      }
    }, POLL_MS);
    onCleanup(() => clearInterval(handle));
  });

  const onlineBadge = (online: boolean) => (
    <StatusBadge
      tone={online ? "active" : "disabled"}
      label={online ? t("fleet.online") : t("fleet.offline")}
    />
  );

  const configBadge = (row: FleetStore) => (
    <StatusBadge
      tone={row.config_current ? "active" : "neutral"}
      label={row.config_current ? t("fleet.inSync") : t("fleet.drifted")}
    />
  );

  const lastSeen = (row: FleetStore) =>
    row.last_seen_at_ms === null ? t("fleet.never") : formatRelativeAge(ageSeconds(row.last_seen_at_ms));

  const columns = (): Column<FleetStore>[] => [
    {
      key: "name",
      header: t("fleet.store"),
      sortValue: (row) => row.name,
      cell: (row) => <span class="font-medium text-ink">{row.name}</span>,
    },
    {
      key: "online",
      header: t("fleet.presence"),
      sortValue: (row) => (row.online ? 1 : 0),
      cell: (row) => onlineBadge(row.online),
    },
    {
      key: "lastSeen",
      header: t("fleet.lastSeen"),
      sortValue: (row) => row.last_seen_at_ms ?? 0,
      cell: (row) => <span class="text-sm text-ink-muted">{lastSeen(row)}</span>,
    },
    {
      key: "config",
      header: t("fleet.config"),
      sortValue: (row) => (row.config_current ? 1 : 0),
      cell: (row) => configBadge(row),
    },
    {
      key: "backlog",
      header: t("fleet.backlog"),
      sortValue: (row) => row.relay_backlog,
      cell: (row) => (
        <div class="flex flex-wrap items-baseline gap-2">
          <span class="text-ink">{formatCount(row.relay_backlog)}</span>
          <Show when={row.relay_backlog > 0 && row.relay_oldest_pending_at_ms !== null}>
            <span class="text-xs text-ink-muted">
              {t("fleet.oldest", {
                age: formatRelativeAge(ageSeconds(row.relay_oldest_pending_at_ms ?? Date.now())),
              })}
            </span>
          </Show>
        </div>
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.store_id}</TechnicalDetails>
      ),
    },
  ];

  // One labelled row in the store-detail drawer.
  const detailRow = (label: string, value: string) => (
    <div class="flex justify-between gap-4 border-b border-line py-2 text-sm last:border-0">
      <span class="text-ink-muted">{label}</span>
      <span class="text-right text-ink">{value}</span>
    </div>
  );

  const healthTone = (entry: TaskHealthEntry): "active" | "disabled" =>
    entry.healthy ? "active" : "disabled";
  const healthLabel = (entry: TaskHealthEntry) =>
    entry.healthy ? t("fleet.healthy") : t("fleet.unhealthy");

  return (
    <div>
      <PageHeader title={t("fleet.title")} description={t("fleet.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Card
            title={t("fleet.systemHealth")}
            actions={
              <Show when={health()}>
                {(report) => (
                  <StatusBadge
                    tone={report().healthy ? "active" : "disabled"}
                    label={report().healthy ? t("fleet.allHealthy") : t("fleet.needsAttention")}
                  />
                )}
              </Show>
            }
          >
            <Show
              when={health()}
              fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}
            >
              {(report) => (
                <Show
                  when={report().tasks.length > 0}
                  fallback={<EmptyState title={t("fleet.noTasks")} />}
                >
                  <ul class="flex flex-col gap-2">
                    <For each={report().tasks}>
                      {(entry) => (
                        <li class="flex flex-wrap items-center justify-between gap-2 rounded-token border border-line px-3 py-2">
                          <div class="flex items-center gap-2">
                            <span class="font-medium text-ink">{taskLabel(entry.task)}</span>
                            <StatusBadge tone={healthTone(entry)} label={healthLabel(entry)} />
                          </div>
                          <span class="text-xs text-ink-muted">
                            {entry.seconds_since === null
                              ? t("fleet.neverTicked")
                              : t("fleet.lastTick", {
                                  age: formatRelativeAge(entry.seconds_since),
                                })}
                          </span>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              )}
            </Show>
          </Card>

          <Card
            title={t("fleet.stores")}
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show
              when={stores()}
              fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={<EmptyState title={t("fleet.empty")} description={t("fleet.emptyHint")} />}
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <Button variant="secondary" onClick={() => setSelected(row)}>
                      {t("fleet.details")}
                    </Button>
                  )}
                />
              )}
            </Show>
          </Card>
        </div>

        <Drawer
          open={selected() !== null}
          title={selected()?.name ?? ""}
          closeLabel={t("action.close")}
          onClose={() => setSelected(null)}
        >
          <Show when={selected()}>
            {(store) => (
              <div class="flex flex-col gap-4">
                <div class="flex flex-wrap gap-2">
                  {onlineBadge(store().online)}
                  {configBadge(store())}
                </div>
                <div>
                  {detailRow(t("fleet.lastSeen"), lastSeen(store()))}
                  {detailRow(
                    t("fleet.lastConfigPull"),
                    store().last_config_pull_at_ms === null
                      ? t("fleet.never")
                      : formatRelativeAge(ageSeconds(store().last_config_pull_at_ms ?? Date.now())),
                  )}
                  {detailRow(t("fleet.versionHeld"), store().config_version_held ?? t("fleet.none"))}
                  {detailRow(
                    t("fleet.versionPublished"),
                    store().config_version_published ?? t("fleet.none"),
                  )}
                  {detailRow(t("fleet.backlog"), formatCount(store().relay_backlog))}
                  {detailRow(
                    t("fleet.oldestPending"),
                    store().relay_oldest_pending_at_ms === null
                      ? t("fleet.none")
                      : formatRelativeAge(
                          ageSeconds(store().relay_oldest_pending_at_ms ?? Date.now()),
                        ),
                  )}
                </div>
                <TechnicalDetails label={t("common.technicalDetails")}>
                  {store().store_id}
                </TechnicalDetails>
              </div>
            )}
          </Show>
        </Drawer>
      </RequireContext>
    </div>
  );
}
