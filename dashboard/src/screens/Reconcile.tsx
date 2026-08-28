// Reconciliation & recovery (ADR-0078, Track O3). Two things in one operational view: the history of
// reconciliation diffs (ADR-0040) — which store reconciled, how many ids it offered, how many the
// cloud was missing, and when — so an operator can finally see that reconciliation ran and what it
// caught; and, for the store in context, the rebuild lever that resets the cloud's rollups so the
// projector re-folds them from the event log (the "reset-cursor-and-replay" recovery). The history
// read is behind console.data.read; the rebuild is behind console.config.publish (the server
// enforces it — a viewer sees the history but a rebuild returns 403).

import { createSignal, onCleanup, onMount, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { ReconcileRun, Store } from "../api/types";
import { t } from "../i18n";
import { formatCount, formatRelativeAge } from "../lib/format";
import { contextReady, onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import { type Column, ConfirmDialog, DataTable, EmptyState, StatusBadge } from "../components/kit";
import { toast } from "../components/Toast";

/** How often the history re-reads, so a fresh reconciliation shows without a manual refresh. */
const POLL_MS = 30_000;

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

export function Reconcile() {
  const [runs, setRuns] = createSignal<ReconcileRun[] | null>(null);
  const [names, setNames] = createSignal<Map<string, string>>(new Map());
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [confirmReset, setConfirmReset] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      // The history (optionally narrowed to the store in context) and a store-name lookup, together.
      const [loadedRuns, stores] = await Promise.all([
        api.listReconcileRuns(tenantId(), storeId() || undefined),
        api.listStores(tenantId()),
      ]);
      setRuns(loadedRuns);
      setNames(new Map(stores.map((store: Store) => [store.store_id, store.name])));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant/store changes (never with an empty context, F0).
  onScopedContext("tenant", () => void load());

  // Poll while the screen is open, but only once a tenant is chosen.
  onMount(() => {
    const handle = setInterval(() => {
      if (contextReady("tenant") && !busy()) {
        void load();
      }
    }, POLL_MS);
    onCleanup(() => clearInterval(handle));
  });

  // A store's name if the registry knows it, else its raw id (a run for an archived store still shows).
  const storeLabel = (id: string) => names().get(id) ?? id;

  const rebuild = async () => {
    setConfirmReset(false);
    setBusy(true);
    try {
      await api.resetRollups(tenantId(), storeId());
      toast.ok(t("reconcile.rebuilt"));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<ReconcileRun>[] => [
    {
      key: "store",
      header: t("reconcile.store"),
      sortValue: (row) => storeLabel(row.store_id),
      cell: (row) => <span class="font-medium text-ink">{storeLabel(row.store_id)}</span>,
    },
    {
      key: "offered",
      header: t("reconcile.offered"),
      sortValue: (row) => row.candidates_offered,
      cell: (row) => <span class="text-ink">{formatCount(row.candidates_offered)}</span>,
    },
    {
      key: "missing",
      header: t("reconcile.missing"),
      sortValue: (row) => row.missing_found,
      cell: (row) => (
        <StatusBadge
          tone={row.missing_found > 0 ? "neutral" : "active"}
          label={
            row.missing_found > 0
              ? t("reconcile.repushed", { count: formatCount(row.missing_found) })
              : t("reconcile.inSync")
          }
        />
      ),
    },
    {
      key: "when",
      header: t("reconcile.when"),
      sortValue: (row) => row.ran_at_ms,
      cell: (row) => (
        <span class="text-sm text-ink-muted">{formatRelativeAge(ageSeconds(row.ran_at_ms))}</span>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("reconcile.title")} description={t("reconcile.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          <Card
            title={
              storeId() ? t("reconcile.historyForStore", { store: storeName() }) : t("reconcile.history")
            }
            actions={
              <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            }
          >
            <Show
              when={runs()}
              fallback={<p class="text-sm text-ink-muted">{t("common.loading")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => storeLabel(row.store_id)}
                  pageSize={15}
                  empty={
                    <EmptyState
                      title={t("reconcile.empty")}
                      description={t("reconcile.emptyHint")}
                    />
                  }
                />
              )}
            </Show>
          </Card>

          <Card title={t("reconcile.rebuild")}>
            <Show
              when={storeId()}
              fallback={
                <EmptyState
                  title={t("reconcile.pickStore")}
                  description={t("reconcile.pickStoreHint")}
                />
              }
            >
              <div class="flex flex-col gap-3">
                <p class="text-sm text-ink-muted">
                  {t("reconcile.rebuildHint", { store: storeName() })}
                </p>
                <div>
                  <Button
                    variant="danger"
                    disabled={busy()}
                    onClick={() => setConfirmReset(true)}
                  >
                    {t("reconcile.rebuildAction")}
                  </Button>
                </div>
              </div>
            </Show>
          </Card>
        </div>

        <ConfirmDialog
          open={confirmReset()}
          danger
          title={t("reconcile.confirmRebuild")}
          message={t("reconcile.confirmRebuildBody", { store: storeName() })}
          confirmLabel={t("reconcile.rebuildAction")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          busy={busy()}
          onConfirm={() => void rebuild()}
          onCancel={() => setConfirmReset(false)}
        />
      </RequireContext>
    </div>
  );
}
