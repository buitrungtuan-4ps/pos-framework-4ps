// The printer/KDS onboarding queue (ADR-0041): a store reports the devices it found on its network;
// the super-admin approves or rejects each pending proposal here. Tenant-scoped, on the authoring
// kit ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)).
//
// Below the queue sit the two store-scoped cards ADR-0112 needs. A terminal is *created* rather than
// proposed — nothing on a LAN announces itself as a till — and an approved printer may then be
// pointed at one, which makes that terminal's agent the thing that writes its bytes. Both halves are
// approved devices in one store, so neither can be shown by the pending queue above; they read
// through `listStoreDevices` and follow the top bar's store, not the tenant.

import { createSignal, Show } from "solid-js";

import { api } from "../api/client";
import type { DeviceProposalSummary, Station, Store } from "../api/types";
import { t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, SelectField, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  EmptyState,
  FormPanel,
  TechnicalDetails,
} from "../components/kit";
import { useEntityCrud } from "../lib/entity-crud";
import { toast } from "../components/Toast";
import { AuditTrail } from "../components/AuditTrail";
import { apiMessage, withStaleReload } from "../lib/errors";

export function Devices() {
  const [rows, setRows] = createSignal<DeviceProposalSummary[] | null>(null);
  // A proposal carries only its store's ULID; the registry (ADR-0065) supplies the name, so the
  // operator reads "Bến Thành" rather than a raw `01J9…`. Fetched alongside the proposals.
  const [names, setNames] = createSignal<Map<string, string>>(new Map());
  const [error, setError] = createSignal("");
  const [loading, setLoading] = createSignal(false);
  // Four lifecycles, one per thing an operator can be in the middle of, so a slow approval does not
  // grey out the terminal card underneath it (ADR-0121 §3).
  //
  // A proposal has no friendly name, so rejection is a plain danger confirm (no type-to-confirm).
  // Approving is not a plain confirm either: it is where an operator states the two facts the store
  // could not discover (ADR-0100). How the device is attached decides whether a cash drawer may be
  // opened at all, and which station it serves decides where a fired line's ticket goes — so it is a
  // form, and it says so by being one.
  const approval = useEntityCrud<DeviceProposalSummary>();
  const rejection = useEntityCrud<DeviceProposalSummary>();
  const terminalDraft = useEntityCrud<never>();
  const agentDraft = useEntityCrud<DeviceProposalSummary>();
  const [connection, setConnection] = createSignal("network");
  const [station, setStation] = createSignal("");
  const [stations, setStations] = createSignal<Station[]>([]);
  // The chosen store's *approved* devices — terminals and printers in one read, because a binding
  // needs both sides and they come from the same list (ADR-0112). `null` is "not loaded yet", which
  // is not the same as a store with no devices.
  const [fleet, setFleet] = createSignal<DeviceProposalSummary[] | null>(null);
  const [terminalName, setTerminalName] = createSignal("");
  // `agentDraft.subject()` is the printer whose agent is being picked, carried with the version it
  // was read at: the write is conditional on that version (ADR-0094), so re-reading the row is what
  // makes a stale pick fail loudly rather than quietly overwrite a colleague's.
  const [agentChoice, setAgentChoice] = createSignal("");

  // The store's registered name, or the raw ULID if the registry has no row for it (a proposal can
  // name a store that predates the backfill, or one already archived).
  const storeName = (storeId: string) => names().get(storeId) ?? storeId;

  const load = async () => {
    setError("");
    setLoading(true);
    try {
      const [proposals, stores] = await Promise.all([
        api.listProposals(tenantId()),
        api.listStores(tenantId()),
      ]);
      setNames(new Map(stores.map((store: Store) => [store.store_id, store.name])));
      setRows(proposals);
    } catch (caught) {
      setError(apiMessage(caught));
    } finally {
      setLoading(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const loadFleet = async () => {
    try {
      setFleet(await api.listStoreDevices(tenantId(), storeId(), "approved"));
    } catch (caught) {
      setError(apiMessage(caught));
    }
  };

  // The two cards below follow the *store* in the top bar, not the tenant: an agent may only be a
  // terminal standing in the same shop, so a tenant-wide list would offer picks the publish gate
  // will refuse.
  onScopedContext("store", () => void loadFleet());

  // Binding a printer to an agent is a conditional write — it sends the version the row was read at
  // (ADR-0094) — so it owes the reader a reload and a sentence when somebody else got there first.
  const conditionalFleet = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, loadFleet, t("devices.stale"));

  const terminals = () => (fleet() ?? []).filter((device) => device.kind === "terminal");
  const printers = () => (fleet() ?? []).filter((device) => device.kind === "printer");

  // A printer names its agent by id; the operator reads the terminal's name. A pick that no longer
  // resolves — the terminal was created in another store, or archived — shows the raw id rather than
  // claiming there is no agent, because "none" is a different and much quieter state.
  const agentLabel = (agentId: string | null) => {
    if (!agentId) {
      return t("devices.agentNone");
    }
    return terminals().find((device) => device.id === agentId)?.name ?? agentId;
  };

  const createTerminal = () => {
    const name = terminalName().trim();
    if (!name) {
      terminalDraft.refuse(t("devices.terminalNameRequired"));
      return;
    }
    void terminalDraft
      .run(() => api.createTerminal(tenantId(), storeId(), name))
      .then((created) => {
        if (created) {
          toast.ok(t("devices.terminalCreated"));
          setTerminalName("");
          void loadFleet();
        }
      });
  };

  const saveAgent = () => {
    const printer = agentDraft.subject();
    if (!printer) {
      return;
    }
    void agentDraft
      .run(() =>
        conditionalFleet(() =>
          api.setPrintAgent(tenantId(), printer.id, agentChoice() || null, printer.version),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("devices.agentSaved"));
          void loadFleet();
        }
      });
  };

  // Opens the approval dialog, pulling the *proposing store's* stations so the picker offers real
  // names rather than asking for a ULID. A store with no stations configured still approves — the
  // device is then the counter's receipt printer, which serves the bill and no station.
  const startApprove = async (row: DeviceProposalSummary) => {
    setConnection("network");
    setStation("");
    approval.edit(row);
    try {
      setStations(await api.listStations(tenantId(), row.store_id));
    } catch {
      // A station list that will not load is not a reason to block approval: the picker simply
      // offers nothing, and the device approves as the counter's printer.
      setStations([]);
    }
  };

  const approve = () => {
    const proposal = approval.subject();
    if (!proposal) {
      return;
    }
    void approval
      .run(() => api.approveDevice(tenantId(), proposal.id, connection(), station() || undefined))
      .then((approved) => {
        if (approved) {
          toast.ok(t("devices.approved"));
          void load();
        }
      });
  };

  const reject = () => {
    const proposal = rejection.subject();
    if (!proposal) {
      return;
    }
    void rejection.run(() => api.rejectDevice(tenantId(), proposal.id)).then((rejected) => {
      if (rejected) {
        toast.ok(t("devices.rejected"));
        void load();
      } else {
        // The confirm stays open carrying the refusal; a page banner would say it twice.
        toast.error(rejection.error());
      }
    });
  };

  const columns = (): Column<DeviceProposalSummary>[] => [
    // The two facts that identify the thing being approved (production-readiness O3). The cloud has
    // always served them and this screen dropped both, so an operator was asked to approve a kind
    // and a ULID — with no way to tell the counter's printer from the oven's.
    {
      key: "name",
      header: t("devices.name"),
      cell: (row) => <span class="text-ink">{row.name}</span>,
      sortValue: (row) => row.name,
    },
    {
      key: "address",
      header: t("devices.address"),
      cell: (row) => <span class="font-mono text-sm text-ink-muted">{row.address}</span>,
      sortValue: (row) => row.address,
    },
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

  const terminalColumns = (): Column<DeviceProposalSummary>[] => [
    {
      key: "name",
      header: t("devices.name"),
      cell: (row) => <span class="text-ink">{row.name}</span>,
      sortValue: (row) => row.name,
    },
    {
      key: "id",
      header: t("devices.id"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>
          <div>{row.id}</div>
        </TechnicalDetails>
      ),
    },
  ];

  const printerColumns = (): Column<DeviceProposalSummary>[] => [
    {
      key: "name",
      header: t("devices.name"),
      cell: (row) => <span class="text-ink">{row.name}</span>,
      sortValue: (row) => row.name,
    },
    {
      key: "address",
      header: t("devices.address"),
      cell: (row) => <span class="font-mono text-sm text-ink-muted">{row.address}</span>,
      sortValue: (row) => row.address,
    },
    {
      key: "agent",
      header: t("devices.agent"),
      cell: (row) => (
        <span class={row.agent_device_id ? "text-ink" : "text-ink-muted"}>
          {agentLabel(row.agent_device_id)}
        </span>
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
            <Button variant="secondary" disabled={loading()} onClick={() => void load()}>
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
                searchText={(row) => `${row.name} ${row.address} ${storeName(row.store_id)} ${row.kind}`}
                pageSize={12}
                empty={<EmptyState title={t("devices.empty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex gap-2">
                    <Button onClick={() => void startApprove(row)}>{t("action.approve")}</Button>
                    <Button variant="danger" onClick={() => rejection.confirm(row)}>
                      {t("action.reject")}
                    </Button>
                  </div>
                )}
              />
            )}
          </Show>
        </Card>

        <div class="mt-6">
          <Card
            title={t("devices.terminals")}
            actions={
              <Button
                disabled={!storeId()}
                onClick={() => {
                  setTerminalName("");
                  terminalDraft.create();
                }}
              >
                {t("devices.newTerminal")}
              </Button>
            }
          >
            <p class="mb-3 text-sm text-ink-muted">{t("devices.terminalsHint")}</p>
            <Show when={storeId()} fallback={<p class="text-sm text-ink-muted">{t("context.storeRequired")}</p>}>
              <DataTable
                columns={terminalColumns()}
                rows={terminals()}
                pageSize={8}
                empty={<EmptyState title={t("devices.terminalsEmpty")} />}
              />
            </Show>
          </Card>
        </div>

        <div class="mt-6">
          <Card title={t("devices.agents")}>
            <p class="mb-3 text-sm text-ink-muted">{t("devices.agentsHint")}</p>
            <Show when={storeId()} fallback={<p class="text-sm text-ink-muted">{t("context.storeRequired")}</p>}>
              <DataTable
                columns={printerColumns()}
                rows={printers()}
                pageSize={8}
                empty={<EmptyState title={t("devices.agentsEmpty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <Button
                    variant="secondary"
                    onClick={() => {
                      setAgentChoice(row.agent_device_id ?? "");
                      agentDraft.edit(row);
                    }}
                  >
                    {t("devices.chooseAgent")}
                  </Button>
                )}
              />
            </Show>
          </Card>
        </div>

        <div class="mt-6">
          <Card title={t("devices.resolved")}>
            <p class="mb-3 text-sm text-ink-muted">{t("devices.resolvedHint")}</p>
            <AuditTrail entityType="device_proposal" />
          </Card>
        </div>

        <FormPanel
          crud={approval}
          createTitle={t("devices.approveTitle")}
          editTitle={t("devices.approveTitle")}
          submitLabel={t("action.approve")}
          onSubmit={approve}
          as="modal"
        >
          <p class="text-sm text-ink-muted">{t("devices.approveHint")}</p>
          <SelectField
            label={t("devices.connectionLabel")}
            value={connection()}
            options={[
              { value: "network", label: t("devices.connection.network") },
              { value: "usb", label: t("devices.connection.usb") },
              { value: "serial", label: t("devices.connection.serial") },
            ]}
            onChange={setConnection}
            hint={t("devices.connectionHint")}
          />
          <SelectField
            label={t("devices.stationLabel")}
            value={station()}
            options={stations().map((entry) => ({ value: entry.station_id, label: entry.name }))}
            onChange={setStation}
            placeholder={t("devices.stationNone")}
            hint={t("devices.stationHint")}
          />
        </FormPanel>

        <FormPanel
          crud={terminalDraft}
          createTitle={t("devices.newTerminalTitle")}
          editTitle={t("devices.newTerminalTitle")}
          submitLabel={t("action.create")}
          onSubmit={createTerminal}
          as="modal"
          dirty={() => terminalName() !== ""}
        >
          <p class="text-sm text-ink-muted">{t("devices.newTerminalHint")}</p>
          <TextField
            label={t("devices.terminalNameLabel")}
            value={terminalName()}
            onInput={setTerminalName}
            hint={t("devices.terminalNameHint")}
          />
        </FormPanel>

        <FormPanel
          crud={agentDraft}
          createTitle={t("devices.agentTitle")}
          editTitle={t("devices.agentTitle")}
          submitLabel={t("action.save")}
          onSubmit={saveAgent}
          as="modal"
        >
          <p class="text-sm text-ink-muted">{t("devices.agentHint")}</p>
          <SelectField
            label={t("devices.agentLabel")}
            value={agentChoice()}
            options={terminals().map((entry) => ({ value: entry.id, label: entry.name }))}
            onChange={setAgentChoice}
            placeholder={t("devices.agentNone")}
          />
        </FormPanel>

        <ConfirmDialog
          open={rejection.mode() === "confirming"}
          title={t("devices.rejectTitle")}
          message={t("devices.rejectMessage")}
          confirmLabel={t("action.reject")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={rejection.saving()}
          onConfirm={reject}
          onCancel={rejection.close}
        />
      </RequireContext>
    </div>
  );
}
