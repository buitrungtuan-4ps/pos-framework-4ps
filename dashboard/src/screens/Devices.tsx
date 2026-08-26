// The printer/KDS onboarding queue (ADR-0041): a store reports the devices it found on its network;
// the super-admin approves or rejects each pending proposal here. Tenant-scoped, on the F2 CRUD kit.

import { createSignal, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { DeviceProposalSummary, Store } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import { type Column, ConfirmDialog, DataTable, EmptyState, TechnicalDetails } from "../components/kit";
import { toast } from "../components/Toast";

export function Devices() {
  const [rows, setRows] = createSignal<DeviceProposalSummary[] | null>(null);
  // A proposal carries only its store's ULID; the registry (ADR-0065) supplies the name, so the
  // operator reads "Bến Thành" rather than a raw `01J9…`. Fetched alongside the proposals.
  const [names, setNames] = createSignal<Map<string, string>>(new Map());
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // A proposal has no friendly name, so rejection is a plain danger confirm (no type-to-confirm).
  const [pendingReject, setPendingReject] = createSignal<DeviceProposalSummary | null>(null);

  // The store's registered name, or the raw ULID if the registry has no row for it (a proposal can
  // name a store that predates the backfill, or one already archived).
  const storeName = (storeId: string) => names().get(storeId) ?? storeId;

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [proposals, stores] = await Promise.all([
        api.listProposals(tenantId()),
        api.listStores(tenantId()),
      ]);
      setNames(new Map(stores.map((store: Store) => [store.store_id, store.name])));
      setRows(proposals);
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const decide = async (id: string, approve: boolean) => {
    setBusy(true);
    try {
      await (approve ? api.approveDevice(tenantId(), id) : api.rejectDevice(tenantId(), id));
      toast.ok(approve ? t("devices.approved") : t("devices.rejected"));
      setPendingReject(null);
      await load();
    } catch (caught) {
      const message = caught instanceof ApiError ? caught.message : String(caught);
      setError(message);
      toast.error(message);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<DeviceProposalSummary>[] => [
    {
      key: "store",
      header: t("devices.store"),
      cell: (row) => <span>{storeName(row.store_id)}</span>,
    },
    {
      key: "kind",
      header: t("devices.kind"),
      cell: (row) => row.kind,
    },
    {
      key: "id",
      header: t("devices.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>
          <div>{row.id}</div>
          <div>{row.store_id}</div>
        </TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("devices.title")} description={t("devices.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("devices.pending")}
          actions={
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={rows()}>
            {(loaded) => (
              <DataTable
                columns={columns()}
                rows={loaded()}
                empty={<EmptyState title={t("devices.empty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex gap-2">
                    <Button disabled={busy()} onClick={() => void decide(row.id, true)}>
                      {t("action.approve")}
                    </Button>
                    <Button variant="danger" disabled={busy()} onClick={() => setPendingReject(row)}>
                      {t("action.reject")}
                    </Button>
                  </div>
                )}
              />
            )}
          </Show>
        </Card>

        <ConfirmDialog
          open={pendingReject() !== null}
          title={t("devices.rejectTitle")}
          message={t("devices.rejectMessage")}
          confirmLabel={t("action.reject")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => {
            const proposal = pendingReject();
            if (proposal) {
              void decide(proposal.id, false);
            }
          }}
          onCancel={() => setPendingReject(null)}
        />
      </RequireContext>
    </div>
  );
}
