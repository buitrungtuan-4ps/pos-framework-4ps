// Reconciliation & recovery (ADR-0078, Track O3). Two things in one operational view: the history of
// reconciliation diffs (ADR-0040) — which store reconciled, how many ids it offered, how many the
// cloud was missing, and when — so an operator can finally see that reconciliation ran and what it
// caught; and, for the store in context, the rebuild lever that resets the cloud's rollups so the
// projector re-folds them from the event log (the "reset-cursor-and-replay" recovery). The history
// read is behind console.data.read; the rebuild is behind console.config.publish (the server
// enforces it — a viewer sees the history but a rebuild returns 403).

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { ReconcileRun, Store } from "../api/types";
import { t } from "../i18n";
import { formatCount, formatRelativeAge } from "../lib/format";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, Skeleton, StatusBadge } from "../components/ui";
import { type Column, ConfirmDialog, DataTable, EmptyState } from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";

/** How often the history re-reads, so a fresh reconciliation shows without a manual refresh. */
const POLL_MS = 30_000;

/** The age, in whole seconds, of a Unix-ms instant against the browser clock (clamped at zero). */
function ageSeconds(atMs: number): number {
  return Math.max(0, (Date.now() - atMs) / 1000);
}

export function Reconcile() {
  const [rebuilding, setRebuilding] = createSignal(false);
  const [confirmReset, setConfirmReset] = createSignal(false);

  // The history (narrowed to the store in context when there is one) and a store-name lookup,
  // together: one state for two reads that are useless apart, so a half-loaded screen cannot show
  // runs with raw ULIDs where names belong. It re-reads itself on the interval and on focus, which
  // is what a screen watching something that runs on a schedule needs (D5).
  const history = createAdminResource(
    async (tenant, store) => {
      const [runs, stores] = await Promise.all([
        api.listReconcileRuns(tenant, store || undefined),
        api.listStores(tenant),
      ]);
      return {
        runs,
        names: new Map(stores.map((row: Store) => [row.store_id, row.name])),
      };
    },
    { scope: "tenant", intervalMs: POLL_MS, revalidateOnFocus: true },
  );

  // A store's name if the registry knows it, else its raw id (a run for an archived store still shows).
  const storeLabel = (id: string) => history.value()?.names.get(id) ?? id;

  const rebuild = async () => {
    setConfirmReset(false);
    setRebuilding(true);
    try {
      await api.resetRollups(tenantId(), storeId());
      toast.ok(t("reconcile.rebuilt"));
      // The rebuild re-folds the rollups, so the next reconciliation has something new to say.
      await history.refetch();
    } catch (caught) {
      toast.error(apiMessage(caught));
    } finally {
      setRebuilding(false);
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
          <Show when={failureOf(history)}>
            {(message) => <Banner tone="danger" message={message()} />}
          </Show>

          <Card
            title={
              storeId() ? t("reconcile.historyForStore", { store: storeName() }) : t("reconcile.history")
            }
          >
            <Show
              when={history.value()}
              fallback={<Skeleton label={t("common.loading")} rows={4} />}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded().runs}
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
                    variant="danger-ghost"
                    disabled={rebuilding()}
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
          busy={rebuilding()}
          onConfirm={() => void rebuild()}
          onCancel={() => setConfirmReset(false)}
        />
      </RequireContext>
    </div>
  );
}
