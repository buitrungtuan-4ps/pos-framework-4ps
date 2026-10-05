// The printer/KDS onboarding queue (ADR-0041): a store reports the devices it found on its network;
// the super-admin approves or rejects each pending proposal here. Tenant-scoped, on the authoring
// kit ([ADR-0121](../../../docs/adr/0121-one-way-to-author-an-entity.md)).
//
// Below the queue sit the two store-scoped cards ADR-0112 needs. A terminal is *created* rather than
// proposed — nothing on a LAN announces itself as a till — and an approved printer may then be
// pointed at one, which makes that terminal's agent the thing that writes its bytes. Both halves are
// approved devices in one store, so neither can be shown by the pending queue above; they read
// through `listStoreDevices` and follow the top bar's store, not the tenant.
//
// The printers card also carries each printer's paper (ADR-0160), the cash drawer mark (ADR-0165)
// and the publish. Nothing decided on this page reaches a store until its devices are published: not
// an approval, not an agent, not a paper, not a drawer, not a till's receipts. So the page offers the
// publish rather than leaving it to a route nobody can see.
//
// The terminals card carries each till's receipts (ADR-0160 decision 4): the printer its receipts,
// receipt copies and pre-bills go to, and the languages they print in, each the store's until
// somebody says. A store on a release older than the one that honours them ignores them, so for that
// store they are hidden and one line says why (decision 5, honour or hide), from the release the
// fleet read says the store runs; a store whose release is unknown is shown them with a note. A
// printer's paper follows the same rule on the printers card, where only the action is hidden: the
// Paper column still shows what was saved, and the line says why it is not in force.

import { createMemo, createSignal, Show } from "solid-js";

import { api } from "../api/client";
import { PAPER_WIDTHS } from "../api/types";
import type { DeviceProposalSummary, PaperWidth, Station, Store } from "../api/types";
import { type MessageKey, t, tFromServer } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { type ReleaseStanding, releaseStanding } from "../lib/semver";
import { storeId, storeName as chosenStoreName, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  SelectField,
  Skeleton,
  TextField,
} from "../components/ui";
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

// What each paper is called on screen (ADR-0160). 80 mm at 42 characters a line is what every
// printer was taken to be before an operator could say, so it is what a printer nobody set shows.
const PAPER_LABELS: Record<PaperWidth, MessageKey> = {
  PAPER_WIDTH_MILLIMETRES_80: "devices.paper.millimetres80",
  PAPER_WIDTH_MILLIMETRES_80_COLUMNS_48: "devices.paper.millimetres80Columns48",
  PAPER_WIDTH_MILLIMETRES_58: "devices.paper.millimetres58",
};
const UNSET_PAPER: PaperWidth = "PAPER_WIDTH_MILLIMETRES_80";
const paperLabel = (paper: PaperWidth | null) =>
  t(PAPER_LABELS[paper ?? UNSET_PAPER] ?? PAPER_LABELS[UNSET_PAPER]);

// The first release whose edge prints a till's receipts at its own printer and in its own languages
// (ADR-0160 decision 4). An older edge ignores both.
const TILL_RECEIPTS_SINCE = "0.14.1";

// The first release whose edge lays a receipt out for the paper a printer takes, and sends no cut
// to one with no cutter (ADR-0160 decision 2). An older edge prints 80 mm paper with a cutter
// whatever the console says.
const PAPER_SINCE = "0.14.1";

// A till's receipt languages: the receipt language setting's own choices, and the second
// language's, each beside "the store's", which is the empty choice.
const RECEIPT_LANGUAGES = [
  "RECEIPT_LANGUAGE_DISPLAY",
  "RECEIPT_LANGUAGE_COUNTRY",
  "RECEIPT_LANGUAGE_VI",
  "RECEIPT_LANGUAGE_EN",
];
const RECEIPT_SECOND_LANGUAGES = [
  "RECEIPT_SECOND_LANGUAGE_NONE",
  "RECEIPT_SECOND_LANGUAGE_VI",
  "RECEIPT_SECOND_LANGUAGE_EN",
];

// A language token in words: the store's when nobody has said, and the token itself for one a newer
// cloud knows and this console does not.
const languageLabel = (token: string | null) =>
  token === null ? t("devices.receiptLanguageStore") : tFromServer(`settings.value.${token}`, token);

export function Devices() {
  const [rows, setRows] = createSignal<DeviceProposalSummary[] | null>(null);
  // A proposal carries only its store's ULID; the registry (ADR-0065) supplies the name, so the
  // operator reads "Bến Thành" rather than a raw `01J9…`. Fetched alongside the proposals.
  const [names, setNames] = createSignal<Map<string, string>>(new Map());
  const [error, setError] = createSignal("");
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
  // The printer whose cash drawer is being marked, read at a version for the same conditional write
  // (ADR-0165). Offered on a USB printer only, because a drawer opens over nothing else.
  const drawerDraft = useEntityCrud<DeviceProposalSummary>();
  const [drawerChoice, setDrawerChoice] = createSignal(false);
  // The printer whose paper is being said, under the same conditional write (ADR-0160). The form
  // opens on what the till takes a printer nobody set to be: 80 mm paper, and a cutter.
  const paperDraft = useEntityCrud<DeviceProposalSummary>();
  const [paperChoice, setPaperChoice] = createSignal<PaperWidth>(UNSET_PAPER);
  const [cutsChoice, setCutsChoice] = createSignal(true);
  // The terminal whose receipts are being said, under the same conditional write (ADR-0160 decision
  // 4). Each picker's empty choice is the store's.
  const receiptDraft = useEntityCrud<DeviceProposalSummary>();
  const [receiptPrinterChoice, setReceiptPrinterChoice] = createSignal("");
  const [receiptLanguageChoice, setReceiptLanguageChoice] = createSignal("");
  const [secondLanguageChoice, setSecondLanguageChoice] = createSignal("");
  // The release the chosen store last reported (ADR-0078), from the fleet read: `undefined` while
  // the read is out, `null` when the store never said or the read failed.
  const [installed, setInstalled] = createSignal<string | null | undefined>(undefined);
  const [publishing, setPublishing] = createSignal(false);

  // The store's registered name, or the raw ULID if the registry has no row for it (a proposal can
  // name a store that predates the backfill, or one already archived).
  const storeName = (storeId: string) => names().get(storeId) ?? storeId;

  const load = async () => {
    setError("");
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

  // Which release the chosen store runs. A fleet that cannot be read costs the release note and not
  // the screen: the store then reads as unknown, which hides nothing, as on Settings.
  const loadRelease = async () => {
    const store = storeId();
    setInstalled(undefined);
    let release: string | null = null;
    try {
      release = (await api.fleetStore(tenantId(), store)).installed_version;
    } catch {
      release = null;
    }
    if (storeId() === store) {
      setInstalled(release);
    }
  };

  // The two cards below follow the *store* in the top bar, not the tenant: an agent may only be a
  // terminal standing in the same shop, so a tenant-wide list would offer picks the publish gate
  // will refuse.
  onScopedContext("store", () => {
    void loadFleet();
    void loadRelease();
  });

  /**
   * Whether the chosen store honours a device's field honoured from release `since` (ADR-0160
   * decision 5, honour or hide), from the release it last reported. `loading` until the fleet read
   * settles, which shows nothing, so a field is never shown and then taken away from an old store.
   */
  const deviceFieldStanding = (since: string): ReleaseStanding | "loading" => {
    const release = installed();
    return release === undefined ? "loading" : releaseStanding(release, since);
  };
  /**
   * Whether a field honoured from release `since` is offered: where the store honours it, and, with
   * a note, where it cannot say.
   */
  const offersDeviceField = (since: string) => {
    const standing = deviceFieldStanding(since);
    return standing === "honours" || standing === "unknown";
  };
  const tillReceipts = () => deviceFieldStanding(TILL_RECEIPTS_SINCE);
  const offersTillReceipts = () => offersDeviceField(TILL_RECEIPTS_SINCE);
  const paperStanding = () => deviceFieldStanding(PAPER_SINCE);
  const offersPaper = () => offersDeviceField(PAPER_SINCE);

  // Binding a printer to an agent is a conditional write — it sends the version the row was read at
  // (ADR-0094) — so it owes the reader a reload and a sentence when somebody else got there first.
  const conditionalFleet = <T,>(write: () => Promise<T>) =>
    withStaleReload(write, loadFleet, t("devices.stale"));

  // Memoized device filters avoid re-filtering `fleet()` on every table render or accessor call.
  const terminals = createMemo(() => (fleet() ?? []).filter((device) => device.kind === "terminal"));
  const printers = createMemo(() => (fleet() ?? []).filter((device) => device.kind === "printer"));
  // Pre-build O(1) lookup map for terminal names by device ID instead of doing O(N) array finds per printer row.
  const terminalMap = createMemo(
    () => new Map(terminals().map((device) => [device.id, device.name])),
  );
  // A till's receipts go to a printer that serves the bill, never a station's.
  const receiptPrinters = createMemo(() => printers().filter((device) => device.station_id === null));

  // A printer names its agent by id; the operator reads the terminal's name. A pick that no longer
  // resolves — the terminal was created in another store, or archived — shows the raw id rather than
  // claiming there is no agent, because "none" is a different and much quieter state.
  const agentLabel = (agentId: string | null) => {
    if (!agentId) {
      return t("devices.agentNone");
    }
    return terminalMap().get(agentId) ?? agentId;
  };

  // A till's receipt printer by name. One that no longer resolves shows its raw id, as an agent
  // does: the store's receipt printer is a different answer, and the publish prints that till at the
  // store's until somebody chooses again.
  const receiptPrinterLabel = (printerId: string | null) => {
    if (!printerId) {
      return t("devices.receiptPrinterStore");
    }
    return printers().find((device) => device.id === printerId)?.name ?? printerId;
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

  const saveDrawer = () => {
    const printer = drawerDraft.subject();
    if (!printer) {
      return;
    }
    void drawerDraft
      .run(() =>
        conditionalFleet(() =>
          api.setDrawerAttached(tenantId(), printer.id, drawerChoice(), printer.version),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("devices.drawerSaved"));
          void loadFleet();
        }
      });
  };

  const savePaper = () => {
    const printer = paperDraft.subject();
    if (!printer) {
      return;
    }
    void paperDraft
      .run(() =>
        conditionalFleet(() =>
          api.setPrinterPaper(tenantId(), printer.id, paperChoice(), cutsChoice(), printer.version),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("devices.paperSaved"));
          void loadFleet();
        }
      });
  };

  const saveReceipt = () => {
    const terminal = receiptDraft.subject();
    if (!terminal) {
      return;
    }
    void receiptDraft
      .run(() =>
        conditionalFleet(() =>
          api.setTerminalReceipt(
            tenantId(),
            terminal.id,
            {
              printerId: receiptPrinterChoice() || null,
              language: receiptLanguageChoice() || null,
              secondLanguage: secondLanguageChoice() || null,
            },
            terminal.version,
          ),
        ),
      )
      .then((saved) => {
        if (saved) {
          toast.ok(t("devices.receiptSaved"));
          void loadFleet();
        }
      });
  };

  const publish = async () => {
    setPublishing(true);
    try {
      const report = await api.publishDevices(tenantId(), storeId());
      toast.ok(t("devices.published", { count: report.device_count }));
      if (report.skipped_count > 0) {
        toast.error(t("devices.publishedSkipped", { count: report.skipped_count }));
      }
    } catch (caught) {
      toast.error(apiMessage(caught));
    } finally {
      setPublishing(false);
    }
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
    ...(offersTillReceipts() ? receiptColumns() : []),
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

  // A till's receipts, offered only where the store's release honours them or cannot say.
  const receiptColumns = (): Column<DeviceProposalSummary>[] => [
    {
      key: "receiptPrinter",
      header: t("devices.receiptPrinter"),
      cell: (row) => (
        <span class={row.receipt_printer_id ? "text-ink" : "text-ink-muted"}>
          {receiptPrinterLabel(row.receipt_printer_id)}
        </span>
      ),
    },
    {
      key: "receiptLanguage",
      header: t("devices.receiptLanguage"),
      cell: (row) => (
        <span class={row.receipt_language ? "text-ink" : "text-ink-muted"}>
          {languageLabel(row.receipt_language)}
        </span>
      ),
    },
    {
      key: "secondLanguage",
      header: t("devices.receiptSecondLanguage"),
      cell: (row) => (
        <span class={row.receipt_second_language ? "text-ink" : "text-ink-muted"}>
          {languageLabel(row.receipt_second_language)}
        </span>
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
    {
      key: "paper",
      header: t("devices.paper"),
      cell: (row) => (
        <span class={row.paper_width ? "text-ink" : "text-ink-muted"}>
          {row.cuts_paper === false
            ? t("devices.paperNoCutter", { paper: paperLabel(row.paper_width) })
            : paperLabel(row.paper_width)}
        </span>
      ),
    },
    {
      key: "drawer",
      header: t("devices.drawer"),
      cell: (row) => (
        <span class={row.drawer_attached ? "text-ink" : "text-ink-muted"}>
          {row.connection !== "usb"
            ? t("devices.drawerUsbOnly")
            : row.drawer_attached
              ? t("devices.drawerAttached")
              : t("devices.drawerNone")}
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
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={rows()} fallback={<Skeleton label={t("common.loading")} rows={4} />}>
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
                    <Button variant="danger-ghost" onClick={() => rejection.confirm(row)}>
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
              <Show when={tillReceipts() === "older"}>
                <p class="mb-3 text-sm text-ink-muted">
                  {t("devices.receiptHidden", {
                    store: chosenStoreName() || storeId(),
                    installed: installed() ?? "",
                    since: TILL_RECEIPTS_SINCE,
                  })}
                </p>
              </Show>
              <Show when={tillReceipts() === "unknown"}>
                <p class="mb-3 text-sm text-ink-muted">
                  {t("devices.receiptUnknown", {
                    store: chosenStoreName() || storeId(),
                    since: TILL_RECEIPTS_SINCE,
                  })}
                </p>
              </Show>
              <DataTable
                columns={terminalColumns()}
                rows={terminals()}
                pageSize={8}
                empty={<EmptyState title={t("devices.terminalsEmpty")} />}
                actionsHeader={offersTillReceipts() ? t("common.actions") : undefined}
                actions={
                  offersTillReceipts()
                    ? (row) => (
                        <Button
                          variant="secondary"
                          onClick={() => {
                            setReceiptPrinterChoice(row.receipt_printer_id ?? "");
                            setReceiptLanguageChoice(row.receipt_language ?? "");
                            setSecondLanguageChoice(row.receipt_second_language ?? "");
                            receiptDraft.edit(row);
                          }}
                        >
                          {t("devices.chooseReceipt")}
                        </Button>
                      )
                    : undefined
                }
              />
            </Show>
          </Card>
        </div>

        <div class="mt-6">
          <Card
            title={t("devices.agents")}
            actions={
              <Button disabled={!storeId() || publishing()} onClick={() => void publish()}>
                {t("devices.publish")}
              </Button>
            }
          >
            <p class="mb-3 text-sm text-ink-muted">{t("devices.agentsHint")}</p>
            <p class="mb-3 text-sm text-ink-muted">{t("devices.publishHint")}</p>
            <Show when={storeId()} fallback={<p class="text-sm text-ink-muted">{t("context.storeRequired")}</p>}>
              <Show when={paperStanding() === "older"}>
                <p class="mb-3 text-sm text-ink-muted">
                  {t("devices.paperHidden", {
                    store: chosenStoreName() || storeId(),
                    installed: installed() ?? "",
                    since: PAPER_SINCE,
                  })}
                </p>
              </Show>
              <Show when={paperStanding() === "unknown"}>
                <p class="mb-3 text-sm text-ink-muted">
                  {t("devices.paperUnknown", {
                    store: chosenStoreName() || storeId(),
                    since: PAPER_SINCE,
                  })}
                </p>
              </Show>
              <DataTable
                columns={printerColumns()}
                rows={printers()}
                pageSize={8}
                empty={<EmptyState title={t("devices.agentsEmpty")} />}
                actionsHeader={t("common.actions")}
                actions={(row) => (
                  <div class="flex gap-2">
                    <Button
                      variant="secondary"
                      onClick={() => {
                        setAgentChoice(row.agent_device_id ?? "");
                        agentDraft.edit(row);
                      }}
                    >
                      {t("devices.chooseAgent")}
                    </Button>
                    <Show when={offersPaper()}>
                      <Button
                        variant="secondary"
                        onClick={() => {
                          setPaperChoice(row.paper_width ?? UNSET_PAPER);
                          setCutsChoice(row.cuts_paper ?? true);
                          paperDraft.edit(row);
                        }}
                      >
                        {t("devices.choosePaper")}
                      </Button>
                    </Show>
                    <Button
                      variant="secondary"
                      disabled={row.connection !== "usb"}
                      onClick={() => {
                        setDrawerChoice(row.drawer_attached);
                        drawerDraft.edit(row);
                      }}
                    >
                      {t("devices.chooseDrawer")}
                    </Button>
                  </div>
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

        <FormPanel
          crud={drawerDraft}
          createTitle={t("devices.drawerTitle")}
          editTitle={t("devices.drawerTitle")}
          submitLabel={t("action.save")}
          onSubmit={saveDrawer}
          as="modal"
        >
          <p class="text-sm text-ink-muted">{t("devices.drawerHint")}</p>
          <CheckboxField
            label={t("devices.drawerLabel")}
            checked={drawerChoice()}
            onChange={setDrawerChoice}
          />
        </FormPanel>

        <FormPanel
          crud={paperDraft}
          createTitle={t("devices.paperTitle")}
          editTitle={t("devices.paperTitle")}
          submitLabel={t("action.save")}
          onSubmit={savePaper}
          as="modal"
        >
          <p class="text-sm text-ink-muted">{t("devices.paperHint")}</p>
          <SelectField
            label={t("devices.paperLabel")}
            value={paperChoice()}
            options={PAPER_WIDTHS.map((paper) => ({ value: paper, label: paperLabel(paper) }))}
            onChange={(value) =>
              setPaperChoice(PAPER_WIDTHS.find((paper) => paper === value) ?? UNSET_PAPER)
            }
          />
          <CheckboxField
            label={t("devices.cutsPaperLabel")}
            checked={cutsChoice()}
            onChange={setCutsChoice}
            hint={t("devices.cutsPaperHint")}
          />
        </FormPanel>

        <FormPanel
          crud={receiptDraft}
          createTitle={t("devices.receiptTitle")}
          editTitle={t("devices.receiptTitle")}
          submitLabel={t("action.save")}
          onSubmit={saveReceipt}
          as="modal"
        >
          <p class="text-sm text-ink-muted">{t("devices.receiptHint")}</p>
          <SelectField
            label={t("devices.receiptPrinter")}
            value={receiptPrinterChoice()}
            options={receiptPrinters().map((entry) => ({ value: entry.id, label: entry.name }))}
            onChange={setReceiptPrinterChoice}
            placeholder={t("devices.receiptPrinterStore")}
          />
          <SelectField
            label={t("devices.receiptLanguage")}
            value={receiptLanguageChoice()}
            options={RECEIPT_LANGUAGES.map((token) => ({ value: token, label: languageLabel(token) }))}
            onChange={setReceiptLanguageChoice}
            placeholder={t("devices.receiptLanguageStore")}
          />
          <SelectField
            label={t("devices.receiptSecondLanguage")}
            value={secondLanguageChoice()}
            options={RECEIPT_SECOND_LANGUAGES.map((token) => ({
              value: token,
              label: languageLabel(token),
            }))}
            onChange={setSecondLanguageChoice}
            placeholder={t("devices.receiptLanguageStore")}
            hint={t("devices.receiptSecondLanguageHint")}
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
