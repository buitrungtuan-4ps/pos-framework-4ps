// The Campaigns authoring screen (ADR-0077, Track M3), on the F2 CRUD kit. An operator authors the
// five campaign kinds — item-level, combo, bill-level, voucher, manual — with their action
// (percentage or amount off), conditions (minimum bill, sales channels, a weekly window), exclusion
// group and quota; mints voucher batches for voucher-kind campaigns; and publishes the tenant's
// campaigns to a store's `campaigns` config node — immediately, after previewing the exact diff, or
// scheduled to a future instant (the Tết-menu case). Campaigns are tenant-authored; publishing,
// previewing and scheduling need a store chosen in the top bar.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import {
  CAMPAIGN_KINDS,
  SALES_CHANNELS,
  type Campaign,
  type CampaignAction,
  type CampaignConditions,
  type CampaignInput,
  type CampaignKind,
  type CampaignPreview,
  type ETag,
  type SalesChannel,
  type ScheduledPublish,
  type Voucher,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import {
  type Column,
  ConfirmDialog,
  DataTable,
  Drawer,
  EmptyState,
  Modal,
  StatusBadge,
  TechnicalDetails,
} from "../components/kit";
import { toast } from "../components/Toast";

/** The five kinds, mapped to their labels. */
const KIND_LABEL: Record<CampaignKind, MessageKey> = {
  item_level: "campaigns.kind.itemLevel",
  combo: "campaigns.kind.combo",
  bill_level: "campaigns.kind.billLevel",
  voucher: "campaigns.kind.voucher",
  manual: "campaigns.kind.manual",
};

/** Per-channel labels, shared with the tax and price editors. */
const CHANNEL_LABEL: Record<SalesChannel, MessageKey> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

/** Weekday labels, Monday first — index i is bit i of the schedule's 7-bit day mask. */
const WEEKDAYS: readonly MessageKey[] = [
  "campaigns.day.mon",
  "campaigns.day.tue",
  "campaigns.day.wed",
  "campaigns.day.thu",
  "campaigns.day.fri",
  "campaigns.day.sat",
  "campaigns.day.sun",
];

/** The largest voucher batch the server mints in one call. */
const MAX_VOUCHER_BATCH = 10_000;

/** Minutes-of-day as an `<input type="time">` value (`930` → `"15:30"`). */
function minutesToTime(total: number): string {
  const hours = Math.floor(total / 60);
  const minutes = total % 60;
  return `${String(hours).padStart(2, "0")}:${String(minutes).padStart(2, "0")}`;
}

/** An `HH:MM` value as minutes of day, or `null` when malformed or out of range. */
function timeToMinutes(text: string): number | null {
  const match = /^(\d{1,2}):(\d{2})$/.exec(text.trim());
  if (!match) {
    return null;
  }
  const hours = Number(match[1]);
  const minutes = Number(match[2]);
  if (!Number.isInteger(hours) || !Number.isInteger(minutes) || hours > 23 || minutes > 59) {
    return null;
  }
  return hours * 60 + minutes;
}

/** A one-line summary of what a campaign takes off, for the list. */
function describeAction(action: CampaignAction): string {
  if (action.type === "percentage") {
    const { numerator, denominator } = action.rate;
    const percent = denominator === 0 ? 0 : (numerator / denominator) * 100;
    return t("campaigns.percentOff", { percent: String(percent) });
  }
  return t("campaigns.amountOff", {
    amount: String(action.amount.amount_minor),
    currency: action.amount.currency_code,
  });
}

export function Campaigns() {
  const [campaigns, setCampaigns] = createSignal<Campaign[] | null>(null);
  const [scheduled, setScheduled] = createSignal<ScheduledPublish[]>([]);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  // The authoring drawer.
  const [formOpen, setFormOpen] = createSignal(false);
  // The campaign being edited, as the id *and* the version it was read at: an update is conditional
  // on that version (ADR-0095), so the two travel together rather than in separate signals that
  // could drift apart.
  const [editing, setEditing] = createSignal<{ id: string; etag: ETag } | null>(null);
  const [fName, setFName] = createSignal("");
  const [fKind, setFKind] = createSignal<CampaignKind>("bill_level");
  const [fPriority, setFPriority] = createSignal("0");
  const [fExclusion, setFExclusion] = createSignal("");
  const [fActionType, setFActionType] = createSignal<"percentage" | "amount_off">("percentage");
  const [fPercent, setFPercent] = createSignal("10");
  const [fAmount, setFAmount] = createSignal("");
  const [fCurrency, setFCurrency] = createSignal("VND");
  const [fMinBill, setFMinBill] = createSignal("");
  const [fChannels, setFChannels] = createSignal<SalesChannel[]>([]);
  const [fScheduleOn, setFScheduleOn] = createSignal(false);
  const [fDays, setFDays] = createSignal<boolean[]>([false, false, false, false, false, false, false]);
  const [fStart, setFStart] = createSignal("00:00");
  const [fEnd, setFEnd] = createSignal("00:00");
  const [fQuota, setFQuota] = createSignal("");

  // Deletion.
  const [pendingDelete, setPendingDelete] = createSignal<Campaign | null>(null);

  // Vouchers.
  const [voucherCampaign, setVoucherCampaign] = createSignal<Campaign | null>(null);
  const [voucherCount, setVoucherCount] = createSignal("50");
  const [existingVouchers, setExistingVouchers] = createSignal<Voucher[]>([]);
  const [mintedVouchers, setMintedVouchers] = createSignal<Voucher[]>([]);

  // Publish / preview / schedule.
  const [preview, setPreview] = createSignal<CampaignPreview | null>(null);
  const [scheduleAt, setScheduleAt] = createSignal("");
  const [pendingCancel, setPendingCancel] = createSignal<ScheduledPublish | null>(null);

  // Errors surface on the page (Banner) and as a transient toast (F1).
  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setCampaigns(await api.listCampaigns(tenantId()));
      setScheduled(storeId() ? await api.listScheduled(tenantId(), storeId()) : []);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant or store changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const openCreate = () => {
    setEditing(null);
    setFName("");
    setFKind("bill_level");
    setFPriority("0");
    setFExclusion("");
    setFActionType("percentage");
    setFPercent("10");
    setFAmount("");
    setFCurrency("VND");
    setFMinBill("");
    setFChannels([]);
    setFScheduleOn(false);
    setFDays([false, false, false, false, false, false, false]);
    setFStart("00:00");
    setFEnd("00:00");
    setFQuota("");
    setFormOpen(true);
  };

  const openEdit = (campaign: Campaign) => {
    setEditing({ id: campaign.id, etag: campaign.etag });
    setFName(campaign.name);
    setFKind(campaign.kind);
    setFPriority(String(campaign.priority));
    setFExclusion(campaign.exclusion_group === undefined ? "" : String(campaign.exclusion_group));
    if (campaign.action.type === "percentage") {
      const { numerator, denominator } = campaign.action.rate;
      setFActionType("percentage");
      setFPercent(String(denominator === 0 ? 0 : (numerator / denominator) * 100));
      setFAmount("");
      setFCurrency("VND");
    } else {
      setFActionType("amount_off");
      setFAmount(String(campaign.action.amount.amount_minor));
      setFCurrency(campaign.action.amount.currency_code);
      setFPercent("10");
    }
    const conditions = campaign.conditions;
    setFMinBill(conditions.min_bill ? String(conditions.min_bill.amount_minor) : "");
    if (conditions.min_bill) {
      setFCurrency(conditions.min_bill.currency_code);
    }
    setFChannels(conditions.channels ? [...conditions.channels] : []);
    const schedule = conditions.schedule;
    setFScheduleOn(Boolean(schedule));
    setFDays(
      Array.from({ length: 7 }, (_, i) => (schedule ? ((schedule.days >> i) & 1) === 1 : false)),
    );
    setFStart(schedule ? minutesToTime(schedule.start_minute) : "00:00");
    setFEnd(schedule ? minutesToTime(schedule.end_minute) : "00:00");
    setFQuota(campaign.quota_remaining === undefined ? "" : String(campaign.quota_remaining));
    setFormOpen(true);
  };

  // Assemble a validated `CampaignInput` from the drawer, or set an error and return null.
  const buildInput = (): CampaignInput | null => {
    const name = fName().trim();
    if (!name) {
      setError(t("campaigns.nameRequired"));
      return null;
    }
    const priority = Number(fPriority().trim());
    if (!Number.isInteger(priority)) {
      setError(t("campaigns.priorityInvalid"));
      return null;
    }
    const currency = fCurrency().trim().toUpperCase();

    let action: CampaignAction;
    if (fActionType() === "percentage") {
      const percent = Number(fPercent().trim());
      if (!Number.isFinite(percent) || percent < 0) {
        setError(t("campaigns.percentInvalid"));
        return null;
      }
      action = { type: "percentage", rate: { numerator: Math.round(percent * 100), denominator: 10_000 } };
    } else {
      const minor = Number(fAmount().trim());
      if (!Number.isInteger(minor) || minor < 0) {
        setError(t("campaigns.amountInvalid"));
        return null;
      }
      if (!/^[A-Z]{3}$/.test(currency)) {
        setError(t("campaigns.currencyInvalid"));
        return null;
      }
      action = { type: "amount_off", amount: { currency_code: currency, amount_minor: minor } };
    }

    let minBill: CampaignConditions["min_bill"];
    if (fMinBill().trim() !== "") {
      const minor = Number(fMinBill().trim());
      if (!Number.isInteger(minor) || minor < 0) {
        setError(t("campaigns.minBillInvalid"));
        return null;
      }
      if (!/^[A-Z]{3}$/.test(currency)) {
        setError(t("campaigns.currencyInvalid"));
        return null;
      }
      minBill = { currency_code: currency, amount_minor: minor };
    }
    const channels = fChannels();
    let schedule: CampaignConditions["schedule"];
    if (fScheduleOn()) {
      const start = timeToMinutes(fStart());
      const end = timeToMinutes(fEnd());
      if (start === null || end === null) {
        setError(t("campaigns.timeInvalid"));
        return null;
      }
      const days = fDays().reduce((mask, on, i) => (on ? mask | (1 << i) : mask), 0);
      if (days === 0) {
        setError(t("campaigns.daysRequired"));
        return null;
      }
      schedule = { days, start_minute: start, end_minute: end };
    }
    const withConditions: CampaignConditions = {
      ...(minBill ? { min_bill: minBill } : {}),
      ...(channels.length > 0 ? { channels } : {}),
      ...(schedule ? { schedule } : {}),
    };

    let exclusionGroup: number | undefined;
    if (fExclusion().trim() !== "") {
      const value = Number(fExclusion().trim());
      if (!Number.isInteger(value) || value < 0 || value > 65_535) {
        setError(t("campaigns.exclusionInvalid"));
        return null;
      }
      exclusionGroup = value;
    }
    let quota: number | undefined;
    if (fQuota().trim() !== "") {
      const value = Number(fQuota().trim());
      if (!Number.isInteger(value) || value < 0) {
        setError(t("campaigns.quotaInvalid"));
        return null;
      }
      quota = value;
    }

    return {
      name,
      kind: fKind(),
      priority,
      ...(exclusionGroup !== undefined ? { exclusion_group: exclusionGroup } : {}),
      action,
      conditions: withConditions,
      ...(quota !== undefined ? { quota_remaining: quota } : {}),
    };
  };

  const save = async () => {
    const input = buildInput();
    if (!input) {
      return;
    }
    setBusy(true);
    try {
      const target = editing();
      if (target) {
        await api.updateCampaign(tenantId(), target.id, target.etag, input);
        toast.ok(t("campaigns.updated"));
      } else {
        await api.createCampaign(tenantId(), input);
        toast.ok(t("campaigns.created"));
      }
      setFormOpen(false);
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    const campaign = pendingDelete();
    if (!campaign) {
      return;
    }
    setBusy(true);
    try {
      await api.deleteCampaign(tenantId(), campaign.id);
      setPendingDelete(null);
      toast.ok(t("campaigns.deleted"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const openVouchers = async (campaign: Campaign) => {
    setVoucherCampaign(campaign);
    setVoucherCount("50");
    setMintedVouchers([]);
    setExistingVouchers([]);
    try {
      setExistingVouchers(await api.listVouchers(tenantId(), campaign.id));
    } catch (caught) {
      fail(caught);
    }
  };

  const generateVouchers = async () => {
    const campaign = voucherCampaign();
    if (!campaign) {
      return;
    }
    const count = Number(voucherCount().trim());
    if (!Number.isInteger(count) || count < 1 || count > MAX_VOUCHER_BATCH) {
      setError(t("campaigns.voucherCountInvalid"));
      return;
    }
    setBusy(true);
    try {
      const minted = await api.generateVouchers(tenantId(), campaign.id, count);
      setMintedVouchers(minted);
      setExistingVouchers(await api.listVouchers(tenantId(), campaign.id));
      toast.ok(t("campaigns.vouchersMinted", { count: String(minted.length) }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const runPreview = async () => {
    setBusy(true);
    try {
      setPreview(await api.previewCampaigns(tenantId(), storeId()));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publish = async () => {
    setBusy(true);
    try {
      const result = await api.publishCampaigns(tenantId(), storeId());
      setPreview(null);
      toast.ok(t("campaigns.published", { store: storeName(), version: result.config_version_id }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const schedule = async () => {
    const value = scheduleAt().trim();
    if (!value) {
      setError(t("campaigns.scheduleTimeRequired"));
      return;
    }
    const ms = new Date(value).getTime();
    if (!Number.isFinite(ms) || ms <= Date.now()) {
      setError(t("campaigns.scheduleTimeFuture"));
      return;
    }
    setBusy(true);
    try {
      await api.scheduleCampaigns(tenantId(), storeId(), ms);
      setScheduleAt("");
      toast.ok(t("campaigns.scheduled", { store: storeName() }));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const cancelScheduled = async () => {
    const row = pendingCancel();
    if (!row) {
      return;
    }
    setBusy(true);
    try {
      await api.cancelScheduled(tenantId(), row.id);
      setPendingCancel(null);
      toast.ok(t("campaigns.scheduleCancelled"));
      await load();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const columns = (): Column<Campaign>[] => [
    {
      key: "name",
      header: t("campaigns.name"),
      sortValue: (row) => row.name,
      cell: (row) => <span class="font-medium text-ink">{row.name}</span>,
    },
    {
      key: "kind",
      header: t("campaigns.kind"),
      cell: (row) => <StatusBadge tone="neutral" label={t(KIND_LABEL[row.kind])} />,
    },
    {
      key: "action",
      header: t("campaigns.action"),
      cell: (row) => <span class="text-ink">{describeAction(row.action)}</span>,
    },
    {
      key: "priority",
      header: t("campaigns.priority"),
      sortValue: (row) => row.priority,
      cell: (row) => <span class="tabular-nums text-ink">{String(row.priority)}</span>,
    },
    {
      key: "quota",
      header: t("campaigns.quota"),
      cell: (row) => (
        <span class="tabular-nums text-ink-muted">
          {row.quota_remaining === undefined ? t("campaigns.unlimited") : String(row.quota_remaining)}
        </span>
      ),
    },
    {
      key: "id",
      header: t("common.technicalDetails"),
      cell: (row) => (
        <TechnicalDetails label={t("common.technicalDetails")}>{row.id}</TechnicalDetails>
      ),
    },
  ];

  return (
    <div>
      <PageHeader title={t("campaigns.title")} description={t("campaigns.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Card
            title={t("campaigns.list")}
            actions={
              <div class="flex gap-2">
                <Button disabled={busy()} onClick={openCreate}>
                  {t("campaigns.new")}
                </Button>
                <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
                  {t("action.refresh")}
                </Button>
              </div>
            }
          >
            <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
            <Show
              when={campaigns()}
              fallback={<p class="text-sm text-ink-muted">{t("campaigns.loadHint")}</p>}
            >
              {(loaded) => (
                <DataTable
                  columns={columns()}
                  rows={loaded()}
                  searchText={(row) => row.name}
                  pageSize={12}
                  empty={
                    <EmptyState
                      title={t("campaigns.empty")}
                      description={t("campaigns.emptyHint")}
                    />
                  }
                  actionsHeader={t("common.actions")}
                  actions={(row) => (
                    <div class="flex flex-wrap gap-2">
                      <Button variant="secondary" disabled={busy()} onClick={() => openEdit(row)}>
                        {t("action.edit")}
                      </Button>
                      <Show when={row.kind === "voucher"}>
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() => void openVouchers(row)}
                        >
                          {t("campaigns.vouchers")}
                        </Button>
                      </Show>
                      <Button variant="danger" disabled={busy()} onClick={() => setPendingDelete(row)}>
                        {t("action.delete")}
                      </Button>
                    </div>
                  )}
                />
              )}
            </Show>
          </Card>

          <Card title={t("campaigns.publishTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("campaigns.publishHint")}</p>
            <Show
              when={storeId()}
              fallback={<p class="text-sm text-ink-muted">{t("campaigns.publishNeedsStore")}</p>}
            >
              <div class="flex flex-col gap-4">
                <p class="text-sm text-ink">{t("campaigns.publishTo", { store: storeName() })}</p>
                <div class="flex flex-wrap gap-2">
                  <Button variant="secondary" disabled={busy()} onClick={() => void runPreview()}>
                    {t("campaigns.preview")}
                  </Button>
                  <Button disabled={busy()} onClick={() => void publish()}>
                    {t("campaigns.publish")}
                  </Button>
                </div>

                <div class="border-t border-line pt-4">
                  <span class="mb-1 block text-sm font-medium text-ink">
                    {t("campaigns.scheduleTitle")}
                  </span>
                  <p class="mb-2 text-sm text-ink-muted">{t("campaigns.scheduleHint")}</p>
                  <div class="flex flex-wrap items-end gap-2">
                    <label class="block">
                      <span class="mb-1 block text-sm font-medium text-ink">
                        {t("campaigns.scheduleWhen")}
                      </span>
                      <input
                        type="datetime-local"
                        class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                        aria-label={t("campaigns.scheduleWhen")}
                        value={scheduleAt()}
                        onInput={(event) => setScheduleAt(event.currentTarget.value)}
                      />
                    </label>
                    <Button variant="secondary" disabled={busy()} onClick={() => void schedule()}>
                      {t("campaigns.schedule")}
                    </Button>
                  </div>

                  <Show
                    when={scheduled().length > 0}
                    fallback={<p class="mt-3 text-sm text-ink-muted">{t("campaigns.noScheduled")}</p>}
                  >
                    <ul class="mt-3 flex flex-col gap-2">
                      <For each={scheduled()}>
                        {(row) => (
                          <li class="flex flex-wrap items-center gap-3 rounded-token border border-line bg-surface-raised px-3 py-2">
                            <StatusBadge
                              tone={
                                row.status === "pending"
                                  ? "active"
                                  : row.status === "applied"
                                    ? "neutral"
                                    : "disabled"
                              }
                              label={t(`campaigns.sched.${row.status}` as MessageKey)}
                            />
                            <span class="text-sm text-ink">
                              {new Date(row.effective_at_ms).toLocaleString()}
                            </span>
                            <Show when={row.status === "pending"}>
                              <Button
                                variant="secondary"
                                disabled={busy()}
                                onClick={() => setPendingCancel(row)}
                              >
                                {t("action.cancel")}
                              </Button>
                            </Show>
                          </li>
                        )}
                      </For>
                    </ul>
                  </Show>
                </div>
              </div>
            </Show>
          </Card>
        </div>

        {/* Authoring drawer */}
        <Drawer
          open={formOpen()}
          title={editing() ? t("campaigns.editTitle") : t("campaigns.newTitle")}
          closeLabel={t("action.close")}
          onClose={() => setFormOpen(false)}
          footer={
            <div class="flex gap-2">
              <Button disabled={busy()} onClick={() => void save()}>
                {t("action.save")}
              </Button>
              <Button variant="secondary" disabled={busy()} onClick={() => setFormOpen(false)}>
                {t("action.cancel")}
              </Button>
            </div>
          }
        >
          <div class="flex flex-col gap-4">
            <TextField
              label={t("campaigns.name")}
              value={fName()}
              onInput={setFName}
              placeholder={t("campaigns.namePlaceholder")}
            />
            <label class="block">
              <span class="mb-1 block text-sm font-medium text-ink">{t("campaigns.kind")}</span>
              <select
                class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                value={fKind()}
                onChange={(event) => setFKind(event.currentTarget.value as CampaignKind)}
              >
                <For each={CAMPAIGN_KINDS}>
                  {(kind) => <option value={kind}>{t(KIND_LABEL[kind])}</option>}
                </For>
              </select>
            </label>
            <div class="grid gap-4 sm:grid-cols-2">
              <TextField
                label={t("campaigns.priority")}
                type="number"
                value={fPriority()}
                onInput={setFPriority}
              />
              <TextField
                label={t("campaigns.exclusionGroup")}
                type="number"
                value={fExclusion()}
                onInput={setFExclusion}
                placeholder={t("campaigns.exclusionPlaceholder")}
              />
            </div>

            <div class="border-t border-line pt-4">
              <label class="block">
                <span class="mb-1 block text-sm font-medium text-ink">{t("campaigns.action")}</span>
                <select
                  class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                  value={fActionType()}
                  onChange={(event) =>
                    setFActionType(event.currentTarget.value as "percentage" | "amount_off")
                  }
                >
                  <option value="percentage">{t("campaigns.actionPercentage")}</option>
                  <option value="amount_off">{t("campaigns.actionAmountOff")}</option>
                </select>
              </label>
              <div class="mt-3">
                <Show
                  when={fActionType() === "percentage"}
                  fallback={
                    <div class="grid gap-4 sm:grid-cols-2">
                      <TextField
                        label={t("campaigns.amountMinor")}
                        type="number"
                        value={fAmount()}
                        onInput={setFAmount}
                        placeholder={t("campaigns.amountPlaceholder")}
                      />
                      <TextField
                        label={t("campaigns.currency")}
                        value={fCurrency()}
                        onInput={setFCurrency}
                        placeholder={t("campaigns.currencyPlaceholder")}
                      />
                    </div>
                  }
                >
                  <TextField
                    label={t("campaigns.percent")}
                    type="number"
                    value={fPercent()}
                    onInput={setFPercent}
                  />
                </Show>
              </div>
            </div>

            <div class="border-t border-line pt-4">
              <span class="mb-2 block text-sm font-medium text-ink">{t("campaigns.conditions")}</span>
              <TextField
                label={t("campaigns.minBill")}
                type="number"
                value={fMinBill()}
                onInput={setFMinBill}
                placeholder={t("campaigns.minBillPlaceholder")}
              />
              <fieldset class="mt-3">
                <legend class="mb-1 text-sm font-medium text-ink">{t("campaigns.channels")}</legend>
                <p class="mb-2 text-sm text-ink-muted">{t("campaigns.channelsHint")}</p>
                <div class="flex flex-wrap gap-3">
                  <For each={SALES_CHANNELS}>
                    {(channel) => (
                      <label class="flex items-center gap-2 text-sm text-ink">
                        <input
                          type="checkbox"
                          class="h-4 w-4"
                          checked={fChannels().includes(channel)}
                          onChange={() =>
                            setFChannels((prev) =>
                              prev.includes(channel)
                                ? prev.filter((c) => c !== channel)
                                : [...prev, channel],
                            )
                          }
                        />
                        {t(CHANNEL_LABEL[channel])}
                      </label>
                    )}
                  </For>
                </div>
              </fieldset>
              <div class="mt-3">
                <label class="flex items-center gap-2 text-sm text-ink">
                  <input
                    type="checkbox"
                    class="h-4 w-4"
                    checked={fScheduleOn()}
                    onChange={(event) => setFScheduleOn(event.currentTarget.checked)}
                  />
                  {t("campaigns.scheduleWindow")}
                </label>
                <Show when={fScheduleOn()}>
                  <div class="mt-2 flex flex-col gap-3">
                    <div class="flex flex-wrap gap-3">
                      <For each={WEEKDAYS}>
                        {(dayKey, index) => (
                          <label class="flex items-center gap-2 text-sm text-ink">
                            <input
                              type="checkbox"
                              class="h-4 w-4"
                              checked={fDays()[index()] ?? false}
                              onChange={() =>
                                setFDays((prev) =>
                                  prev.map((on, i) => (i === index() ? !on : on)),
                                )
                              }
                            />
                            {t(dayKey)}
                          </label>
                        )}
                      </For>
                    </div>
                    <div class="grid gap-4 sm:grid-cols-2">
                      <label class="block">
                        <span class="mb-1 block text-sm font-medium text-ink">
                          {t("campaigns.startTime")}
                        </span>
                        <input
                          type="time"
                          class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                          aria-label={t("campaigns.startTime")}
                          value={fStart()}
                          onInput={(event) => setFStart(event.currentTarget.value)}
                        />
                      </label>
                      <label class="block">
                        <span class="mb-1 block text-sm font-medium text-ink">
                          {t("campaigns.endTime")}
                        </span>
                        <input
                          type="time"
                          class="min-h-touch w-full rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                          aria-label={t("campaigns.endTime")}
                          value={fEnd()}
                          onInput={(event) => setFEnd(event.currentTarget.value)}
                        />
                      </label>
                    </div>
                  </div>
                </Show>
              </div>
              <div class="mt-3">
                <TextField
                  label={t("campaigns.quota")}
                  type="number"
                  value={fQuota()}
                  onInput={setFQuota}
                  placeholder={t("campaigns.quotaPlaceholder")}
                />
              </div>
            </div>
          </div>
        </Drawer>

        {/* Voucher minting modal */}
        <Modal
          open={voucherCampaign() !== null}
          title={t("campaigns.vouchersTitle")}
          closeLabel={t("action.close")}
          onClose={() => setVoucherCampaign(null)}
        >
          <Show when={voucherCampaign()}>
            {(campaign) => (
              <div class="flex flex-col gap-4">
                <p class="text-sm text-ink">{campaign().name}</p>
                <div class="flex flex-wrap items-end gap-2">
                  <TextField
                    label={t("campaigns.voucherCount")}
                    type="number"
                    value={voucherCount()}
                    onInput={setVoucherCount}
                  />
                  <Button disabled={busy()} onClick={() => void generateVouchers()}>
                    {t("campaigns.generate")}
                  </Button>
                </div>
                <Show when={mintedVouchers().length > 0}>
                  <div class="rounded-token border border-line bg-surface-raised p-3">
                    <p class="mb-2 text-sm font-medium text-ink">{t("campaigns.mintedNote")}</p>
                    <div class="max-h-48 overflow-y-auto">
                      <ul class="flex flex-col gap-1">
                        <For each={mintedVouchers()}>
                          {(voucher) => (
                            <li class="font-mono text-sm text-ink">{voucher.code}</li>
                          )}
                        </For>
                      </ul>
                    </div>
                  </div>
                </Show>
                <p class="text-sm text-ink-muted">
                  {t("campaigns.voucherTotal", { count: String(existingVouchers().length) })}
                </p>
              </div>
            )}
          </Show>
        </Modal>

        {/* Preview modal */}
        <Modal
          open={preview() !== null}
          title={t("campaigns.previewTitle")}
          closeLabel={t("action.close")}
          onClose={() => setPreview(null)}
          footer={
            <Button disabled={busy()} onClick={() => void publish()}>
              {t("campaigns.publish")}
            </Button>
          }
        >
          <Show when={preview()}>
            {(result) => (
              <div class="flex flex-col gap-3">
                <p class="text-sm text-ink-muted">
                  {result().from_version_id
                    ? t("campaigns.previewFrom", { version: result().from_version_id ?? "" })
                    : t("campaigns.previewFirst")}
                </p>
                <Show
                  when={!result().unchanged}
                  fallback={<Banner tone="ok" message={t("campaigns.previewUnchanged")} />}
                >
                  <div class="overflow-x-auto rounded-token border border-line bg-surface-raised p-3">
                    <pre class="whitespace-pre text-sm text-ink">
                      {JSON.stringify(result().diff, null, 2)}
                    </pre>
                  </div>
                </Show>
              </div>
            )}
          </Show>
        </Modal>

        <ConfirmDialog
          open={pendingDelete() !== null}
          title={t("campaigns.deleteTitle")}
          message={t("campaigns.deleteMessage")}
          confirmLabel={t("action.delete")}
          cancelLabel={t("action.cancel")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void remove()}
          onCancel={() => setPendingDelete(null)}
        />

        <ConfirmDialog
          open={pendingCancel() !== null}
          title={t("campaigns.cancelTitle")}
          message={t("campaigns.cancelMessage")}
          confirmLabel={t("campaigns.cancelConfirm")}
          cancelLabel={t("action.close")}
          closeLabel={t("action.close")}
          danger
          busy={busy()}
          onConfirm={() => void cancelScheduled()}
          onCancel={() => setPendingCancel(null)}
        />
      </RequireContext>
    </div>
  );
}
