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
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { AuditTrail } from "../components/AuditTrail";

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

/** Friendly labels for the three edge placements (ADR-0110). Same shape and same fallback rule as
 *  TASK_LABELS above: a token this build does not know shows verbatim rather than blank, which is
 *  what a console older than its server should do. */
const PLACEMENT_LABELS: Record<string, MessageKey> = {
  EDGE_PLACEMENT_IN_STORE: "fleet.placement.inStore",
  EDGE_PLACEMENT_HOSTED_BY_OPERATOR: "fleet.placement.hostedByOperator",
  EDGE_PLACEMENT_HOSTED_BY_PLATFORM: "fleet.placement.hostedByPlatform",
};

function placementLabel(token: string): string {
  const key = PLACEMENT_LABELS[token];
  return key ? t(key) : token;
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
  // The lease bump (ADR-0108) is the one destructive act on this screen: it supersedes whatever box
  // holds the store's current generation, which stops it taking updates. It therefore asks for the
  // store's name typed out, the same guard the other irreversible actions use.
  const [bumping, setBumping] = createSignal<FleetStore | null>(null);
  const [bumpBusy, setBumpBusy] = createSignal(false);

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

  // Beside the presence badge, never on a settings page: a store's mode decides what its silence
  // *means*, so it belongs where somebody reads that silence during an incident (ADR-0110).
  // In-store is the fleet's norm and gets no badge — marking every store would say nothing. `null`
  // also gets none: it means the store has never been bumped, or the server could not read its
  // token, and the console is not the place that difference is resolved.
  const placementBadge = (row: FleetStore) => (
    <Show
      when={
        row.edge_placement && row.edge_placement !== "EDGE_PLACEMENT_IN_STORE"
          ? row.edge_placement
          : undefined
      }
    >
      {(token) => <StatusBadge tone="neutral" label={placementLabel(token())} />}
    </Show>
  );

  const configBadge = (row: FleetStore) => (
    <StatusBadge
      tone={row.config_current ? "active" : "neutral"}
      label={row.config_current ? t("fleet.inSync") : t("fleet.drifted")}
    />
  );

  // Shown only when the store is actually under a lease *and* this box is behind it. A store the
  // cloud has never issued a lease to has no standing to display, and putting a badge on it would
  // mark the whole fleet the day this shipped.
  const leaseBadge = (row: FleetStore) => (
    <Show when={row.lease_superseded}>
      <StatusBadge tone="danger" label={t("fleet.leaseSuperseded")} />
    </Show>
  );

  const bumpLease = async (row: FleetStore) => {
    setBumpBusy(true);
    try {
      // The generation this row was read at, as the write's precondition. A store with no lease
      // yet sends `*`. If another admin bumped since this table loaded, the server refuses rather
      // than letting both of us believe we moved the store.
      await api.bumpStoreLease(
        tenantId(),
        row.store_id,
        row.lease_generation_authoritative,
      );
      toast.ok(t("fleet.leaseBumped"));
      setBumping(null);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBumpBusy(false);
    }
  };

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
      cell: (row) => (
        <div class="flex flex-wrap items-center gap-1">
          {onlineBadge(row.online)}
          {placementBadge(row)}
        </div>
      ),
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
      key: "outbox",
      header: t("fleet.outbox"),
      // A store that never reported sorts below every store that did: "did not say" is not a count,
      // and sorting it as zero would bury a silent box among the healthiest rows.
      sortValue: (row) => row.outbox_depth ?? -1,
      cell: (row) => (
        <div class="flex flex-wrap items-baseline gap-2">
          <Show
            when={row.outbox_depth !== null}
            fallback={<span class="text-sm text-ink-muted">{t("fleet.notReported")}</span>}
          >
            <span class="text-ink">{formatCount(row.outbox_depth ?? 0)}</span>
            <Show when={(row.outbox_depth ?? 0) > 0 && row.outbox_reported_at_ms !== null}>
              <span class="text-xs text-ink-muted">
                {t("fleet.reportedAge", {
                  age: formatRelativeAge(ageSeconds(row.outbox_reported_at_ms ?? Date.now())),
                })}
              </span>
            </Show>
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
                  {placementBadge(store())}
                  {configBadge(store())}
                  {leaseBadge(store())}
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
                  {detailRow(
                    t("fleet.outbox"),
                    store().outbox_depth === null
                      ? t("fleet.notReported")
                      : formatCount(store().outbox_depth ?? 0),
                  )}
                  {detailRow(
                    t("fleet.outboxReported"),
                    store().outbox_reported_at_ms === null
                      ? t("fleet.never")
                      : formatRelativeAge(ageSeconds(store().outbox_reported_at_ms ?? Date.now())),
                  )}
                  {/* Both generations, side by side, because either alone is unreadable: the
                      authority says which machine *should* be the store, the held one says which
                      machine this is. They differ exactly when a box has been replaced. */}
                  {detailRow(
                    t("fleet.leaseAuthoritative"),
                    store().lease_generation_authoritative === null
                      ? t("fleet.leaseNone")
                      : formatCount(store().lease_generation_authoritative ?? 0),
                  )}
                  {detailRow(
                    t("fleet.leaseHeld"),
                    store().lease_generation_held === null
                      ? t("fleet.notReported")
                      : formatCount(store().lease_generation_held ?? 0),
                  )}
                </div>
                <div class="flex flex-col gap-2">
                  <Button variant="danger" onClick={() => setBumping(store())}>
                    {t("fleet.leaseBump")}
                  </Button>
                  <p class="text-xs text-ink-muted">{t("fleet.leaseBumpHint")}</p>
                </div>
                <TechnicalDetails label={t("common.technicalDetails")}>
                  {store().store_id}
                </TechnicalDetails>
                <div>
                  <span class="mb-2 block text-sm font-medium text-ink">{t("audit.history")}</span>
                  <AuditTrail entityType="store" entityId={store().store_id} />
                </div>
              </div>
            )}
          </Show>
        </Drawer>

        <ConfirmDialog
          open={bumping() !== null}
          danger
          busy={bumpBusy()}
          title={t("fleet.leaseBump")}
          message={t("fleet.leaseBumpConfirm")}
          confirmLabel={t("fleet.leaseBump")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          typeToConfirm={bumping()?.name ?? ""}
          typePrompt={t("fleet.leaseBumpTypePrompt")}
          onConfirm={() => {
            const target = bumping();
            if (target) {
              void bumpLease(target);
            }
          }}
          onCancel={() => setBumping(null)}
        />
      </RequireContext>
    </div>
  );
}
