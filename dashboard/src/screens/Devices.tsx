// The printer/KDS onboarding queue (ADR-0041): a store reports the devices it found on its network;
// the super-admin approves or rejects each pending proposal here. Tenant-scoped, on the F2 CRUD kit.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { DeviceProposalSummary, Station, Store } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  Modal,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";
import { AuditTrail } from "../components/AuditTrail";

export function Devices() {
  const [rows, setRows] = createSignal<DeviceProposalSummary[] | null>(null);
  // A proposal carries only its store's ULID; the registry (ADR-0065) supplies the name, so the
  // operator reads "Bến Thành" rather than a raw `01J9…`. Fetched alongside the proposals.
  const [names, setNames] = createSignal<Map<string, string>>(new Map());
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // A proposal has no friendly name, so rejection is a plain danger confirm (no type-to-confirm).
  const [pendingReject, setPendingReject] = createSignal<DeviceProposalSummary | null>(null);
  // Approving is not a plain confirm: it is where an operator states the two facts the store could
  // not discover (ADR-0100). How the device is attached decides whether a cash drawer may be opened
  // at all, and which station it serves decides where a fired line's ticket goes.
  const [pendingApprove, setPendingApprove] = createSignal<DeviceProposalSummary | null>(null);
  const [connection, setConnection] = createSignal("network");
  const [station, setStation] = createSignal("");
  const [stations, setStations] = createSignal<Station[]>([]);

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

  // Opens the approval dialog, pulling the *proposing store's* stations so the picker offers real
  // names rather than asking for a ULID. A store with no stations configured still approves — the
  // device is then the counter's receipt printer, which serves the bill and no station.
  const startApprove = async (row: DeviceProposalSummary) => {
    setConnection("network");
    setStation("");
    setPendingApprove(row);
    try {
      setStations(await api.listStations(tenantId(), row.store_id));
    } catch {
      // A station list that will not load is not a reason to block approval: the picker simply
      // offers nothing, and the device approves as the counter's printer.
      setStations([]);
    }
  };

  const decide = async (id: string, approve: boolean) => {
    setBusy(true);
    try {
      await (approve
        ? api.approveDevice(tenantId(), id, connection(), station() || undefined)
        : api.rejectDevice(tenantId(), id));
      toast.ok(approve ? t("devices.approved") : t("devices.rejected"));
      setPendingReject(null);
      setPendingApprove(null);
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
                searchText={(row) => `${storeName(row.store_id)} ${row.kind}`}
                pageSize={12}
                empty={<EmptyState title={t("devices.empty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex gap-2">
                    <Button disabled={busy()} onClick={() => void startApprove(row)}>
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

        <div class="mt-6">
          <Card title={t("devices.resolved")}>
            <p class="mb-3 text-sm text-ink-muted">{t("devices.resolvedHint")}</p>
            <AuditTrail entityType="device_proposal" />
          </Card>
        </div>

        <Modal
          open={pendingApprove() !== null}
          title={t("devices.approveTitle")}
          closeLabel={t("action.close")}
          onClose={() => setPendingApprove(null)}
          footer={
            <Button
              disabled={busy()}
              onClick={() => {
                const proposal = pendingApprove();
                if (proposal) {
                  void decide(proposal.id, true);
                }
              }}
            >
              {t("action.approve")}
            </Button>
          }
        >
          <p class="mb-4 text-sm text-ink-muted">{t("devices.approveHint")}</p>
          <label class="mb-4 block">
            <span class="mb-1 block text-sm font-medium text-ink">
              {t("devices.connectionLabel")}
            </span>
            <select
              class="w-full rounded-token border border-line bg-surface px-2 py-1.5 text-sm text-ink"
              value={connection()}
              onChange={(event) => setConnection(event.currentTarget.value)}
            >
              <option value="network">{t("devices.connection.network")}</option>
              <option value="usb">{t("devices.connection.usb")}</option>
              <option value="serial">{t("devices.connection.serial")}</option>
            </select>
            <span class="mt-1 block text-xs text-ink-muted">{t("devices.connectionHint")}</span>
          </label>
          <label class="block">
            <span class="mb-1 block text-sm font-medium text-ink">{t("devices.stationLabel")}</span>
            <select
              class="w-full rounded-token border border-line bg-surface px-2 py-1.5 text-sm text-ink"
              value={station()}
              onChange={(event) => setStation(event.currentTarget.value)}
            >
              <option value="">{t("devices.stationNone")}</option>
              <For each={stations()}>
                {(entry) => <option value={entry.station_id}>{entry.name}</option>}
              </For>
            </select>
            <span class="mt-1 block text-xs text-ink-muted">{t("devices.stationHint")}</span>
          </label>
        </Modal>

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
