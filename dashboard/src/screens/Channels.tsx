// The Channels & payments screen (ADR-0080, Track M7). Four store-scoped settings, each published to
// one store's config node: which sales channels it accepts (`channels`), which payment methods
// (`tender`), its QR ordering guardrails (`qr`), and its per-marketplace vendor policies (`vendors`).
// Each is opt-in and never-blank — an unpublished node means "no restriction" — so publishing needs a
// store chosen in the top bar. The edge applies channels/tender as gates and qr as its staff-confirm
// source; the live marketplace loop for vendor policy is a flagged follow-up.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import {
  SALES_CHANNELS,
  VENDOR_AVAILABILITIES,
  type QrGuardrails,
  type SalesChannel,
  type VendorAvailability,
  type VendorPolicy,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";
import { toast } from "../components/Toast";

/** Per-channel labels, shared with the tax and campaign editors. */
const CHANNEL_LABEL: Record<SalesChannel, MessageKey> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

/** The payment-method wire tokens the tender editor offers, with their labels. */
const PAYMENT_METHODS: readonly string[] = [
  "PAYMENT_METHOD_CASH",
  "PAYMENT_METHOD_CARD",
  "PAYMENT_METHOD_QR",
  "PAYMENT_METHOD_VOUCHER",
  "PAYMENT_METHOD_GIFT_CARD",
  "PAYMENT_METHOD_OTHER",
];

const TENDER_LABEL: Record<string, MessageKey> = {
  PAYMENT_METHOD_CASH: "channels.tender.cash",
  PAYMENT_METHOD_CARD: "channels.tender.card",
  PAYMENT_METHOD_QR: "channels.tender.qr",
  PAYMENT_METHOD_VOUCHER: "channels.tender.voucher",
  PAYMENT_METHOD_GIFT_CARD: "channels.tender.giftCard",
  PAYMENT_METHOD_OTHER: "channels.tender.other",
};

const AVAILABILITY_LABEL: Record<VendorAvailability, MessageKey> = {
  VENDOR_AVAILABILITY_OPEN: "channels.vendor.open",
  VENDOR_AVAILABILITY_BUSY: "channels.vendor.busy",
  VENDOR_AVAILABILITY_CLOSED: "channels.vendor.closed",
};

/** The QR guardrail defaults, matching the server's `QrConfig::default` (ADR-0057). */
const QR_DEFAULTS: QrGuardrails = {
  enabled: true,
  staff_confirmation_required: true,
  per_table_limit: 10,
  rate_window_secs: 60,
  business_hours: null,
};

export function Channels() {
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const [channels, setChannels] = createSignal<SalesChannel[]>([]);
  const [tender, setTender] = createSignal<string[]>([]);
  const [qr, setQr] = createSignal<QrGuardrails>({ ...QR_DEFAULTS });
  const [qrHoursOn, setQrHoursOn] = createSignal(false);
  const [qrOpen, setQrOpen] = createSignal("0");
  const [qrClose, setQrClose] = createSignal("0");
  const [qrOffset, setQrOffset] = createSignal("0");
  const [vendors, setVendors] = createSignal<VendorPolicy[]>([]);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    if (!storeId()) {
      // The nodes are store-scoped; without a store there is nothing to read.
      setChannels([...SALES_CHANNELS]);
      setTender([...PAYMENT_METHODS]);
      setQr({ ...QR_DEFAULTS });
      setVendors([]);
      return;
    }
    setError("");
    setBusy(true);
    try {
      const [ch, te, qg, vp] = await Promise.all([
        api.readChannels(tenantId(), storeId()),
        api.readTender(tenantId(), storeId()),
        api.readQrGuardrails(tenantId(), storeId()),
        api.readVendorPolicies(tenantId(), storeId()),
      ]);
      // An absent node means "no restriction": show every channel / method enabled.
      setChannels(ch ? [...ch.enabled] : [...SALES_CHANNELS]);
      setTender(te ? [...te.accepted] : [...PAYMENT_METHODS]);
      const guardrails = qg ?? { ...QR_DEFAULTS };
      setQr(guardrails);
      setQrHoursOn(Boolean(guardrails.business_hours));
      setQrOpen(String(guardrails.business_hours?.open_hour ?? 0));
      setQrClose(String(guardrails.business_hours?.close_hour ?? 0));
      setQrOffset(String(guardrails.business_hours?.tz_offset_minutes ?? 0));
      setVendors(vp ? vp.policies.map((policy) => ({ ...policy })) : []);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  onScopedContext("tenant", () => void load());

  const toggleChannel = (channel: SalesChannel) =>
    setChannels((prev) =>
      prev.includes(channel) ? prev.filter((c) => c !== channel) : [...prev, channel],
    );

  const toggleTender = (method: string) =>
    setTender((prev) =>
      prev.includes(method) ? prev.filter((m) => m !== method) : [...prev, method],
    );

  const publishChannels = async () => {
    setBusy(true);
    try {
      await api.publishChannels(tenantId(), storeId(), channels());
      toast.ok(t("channels.published", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publishTender = async () => {
    setBusy(true);
    try {
      await api.publishTender(tenantId(), storeId(), tender());
      toast.ok(t("channels.tenderPublished", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Assemble the QR guardrails from the form, or set an error and return null.
  const buildQr = (): QrGuardrails | null => {
    const limit = Number(qr().per_table_limit);
    const window = Number(qr().rate_window_secs);
    if (!Number.isInteger(limit) || limit < 0 || !Number.isInteger(window) || window < 0) {
      setError(t("channels.qrNumbersInvalid"));
      return null;
    }
    let hours: QrGuardrails["business_hours"] = null;
    if (qrHoursOn()) {
      const open = Number(qrOpen());
      const close = Number(qrClose());
      const offset = Number(qrOffset());
      if (
        !Number.isInteger(open) ||
        !Number.isInteger(close) ||
        open < 0 ||
        open > 23 ||
        close < 0 ||
        close > 23 ||
        !Number.isInteger(offset)
      ) {
        setError(t("channels.qrHoursInvalid"));
        return null;
      }
      hours = { open_hour: open, close_hour: close, tz_offset_minutes: offset };
    }
    return {
      enabled: qr().enabled,
      staff_confirmation_required: qr().staff_confirmation_required,
      per_table_limit: limit,
      rate_window_secs: window,
      business_hours: hours,
    };
  };

  const publishQr = async () => {
    const guardrails = buildQr();
    if (!guardrails) {
      return;
    }
    setBusy(true);
    try {
      await api.publishQrGuardrails(tenantId(), storeId(), guardrails);
      toast.ok(t("channels.qrPublished", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publishVendors = async () => {
    for (const policy of vendors()) {
      if (!policy.vendor.trim()) {
        setError(t("channels.vendorNameRequired"));
        return;
      }
      if (!Number.isInteger(Number(policy.prep_minutes)) || Number(policy.prep_minutes) < 0) {
        setError(t("channels.vendorPrepInvalid"));
        return;
      }
    }
    setBusy(true);
    try {
      await api.publishVendorPolicies(
        tenantId(),
        storeId(),
        vendors().map((policy) => ({ ...policy, prep_minutes: Number(policy.prep_minutes) })),
      );
      toast.ok(t("channels.vendorsPublished", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const updateVendor = (index: number, patch: Partial<VendorPolicy>) =>
    setVendors((prev) => prev.map((policy, i) => (i === index ? { ...policy, ...patch } : policy)));

  const addVendor = () =>
    setVendors((prev) => [
      ...prev,
      {
        vendor: "",
        enabled: true,
        availability: "VENDOR_AVAILABILITY_OPEN",
        prep_minutes: 0,
        suppressed_items: [],
      },
    ]);

  const storeGate = (body: () => unknown) => (
    <Show
      when={storeId()}
      fallback={<p class="text-sm text-ink-muted">{t("channels.needsStore")}</p>}
    >
      {body as never}
    </Show>
  );

  return (
    <div>
      <PageHeader title={t("channels.title")} description={t("channels.description")} />
      <RequireContext need="tenant">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          {/* Channels */}
          <Card title={t("channels.channelsTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("channels.channelsHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-4">
                <div class="flex flex-wrap gap-3">
                  <For each={SALES_CHANNELS}>
                    {(channel) => (
                      <label class="flex items-center gap-2 text-sm text-ink">
                        <input
                          type="checkbox"
                          class="h-4 w-4"
                          checked={channels().includes(channel)}
                          onChange={() => toggleChannel(channel)}
                        />
                        {t(CHANNEL_LABEL[channel])}
                      </label>
                    )}
                  </For>
                </div>
                <div>
                  <Button disabled={busy()} onClick={() => void publishChannels()}>
                    {t("channels.publishChannels")}
                  </Button>
                </div>
              </div>
            ))}
          </Card>

          {/* Tender */}
          <Card title={t("channels.tenderTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("channels.tenderHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-4">
                <div class="flex flex-wrap gap-3">
                  <For each={PAYMENT_METHODS}>
                    {(method) => (
                      <label class="flex items-center gap-2 text-sm text-ink">
                        <input
                          type="checkbox"
                          class="h-4 w-4"
                          checked={tender().includes(method)}
                          onChange={() => toggleTender(method)}
                        />
                        {t(TENDER_LABEL[method] ?? "channels.tender.other")}
                      </label>
                    )}
                  </For>
                </div>
                <div>
                  <Button disabled={busy()} onClick={() => void publishTender()}>
                    {t("channels.publishTender")}
                  </Button>
                </div>
              </div>
            ))}
          </Card>

          {/* QR guardrails */}
          <Card title={t("channels.qrTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("channels.qrHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-3">
                <label class="flex items-center gap-2 text-sm text-ink">
                  <input
                    type="checkbox"
                    class="h-4 w-4"
                    checked={qr().enabled}
                    onChange={(event) => setQr({ ...qr(), enabled: event.currentTarget.checked })}
                  />
                  {t("channels.qrEnabled")}
                </label>
                <label class="flex items-center gap-2 text-sm text-ink">
                  <input
                    type="checkbox"
                    class="h-4 w-4"
                    checked={qr().staff_confirmation_required}
                    onChange={(event) =>
                      setQr({ ...qr(), staff_confirmation_required: event.currentTarget.checked })
                    }
                  />
                  {t("channels.qrStaffConfirm")}
                </label>
                <div class="grid gap-4 sm:grid-cols-2">
                  <TextField
                    label={t("channels.qrPerTableLimit")}
                    type="number"
                    value={String(qr().per_table_limit)}
                    onInput={(value) => setQr({ ...qr(), per_table_limit: Number(value) })}
                  />
                  <TextField
                    label={t("channels.qrRateWindow")}
                    type="number"
                    value={String(qr().rate_window_secs)}
                    onInput={(value) => setQr({ ...qr(), rate_window_secs: Number(value) })}
                  />
                </div>
                <label class="flex items-center gap-2 text-sm text-ink">
                  <input
                    type="checkbox"
                    class="h-4 w-4"
                    checked={qrHoursOn()}
                    onChange={(event) => setQrHoursOn(event.currentTarget.checked)}
                  />
                  {t("channels.qrHoursOn")}
                </label>
                <Show when={qrHoursOn()}>
                  <div class="grid gap-4 sm:grid-cols-3">
                    <TextField
                      label={t("channels.qrOpenHour")}
                      type="number"
                      value={qrOpen()}
                      onInput={setQrOpen}
                    />
                    <TextField
                      label={t("channels.qrCloseHour")}
                      type="number"
                      value={qrClose()}
                      onInput={setQrClose}
                    />
                    <TextField
                      label={t("channels.qrOffset")}
                      type="number"
                      value={qrOffset()}
                      onInput={setQrOffset}
                    />
                  </div>
                </Show>
                <div>
                  <Button disabled={busy()} onClick={() => void publishQr()}>
                    {t("channels.publishQr")}
                  </Button>
                </div>
              </div>
            ))}
          </Card>

          {/* Vendor policies */}
          <Card
            title={t("channels.vendorsTitle")}
            actions={
              <Button variant="secondary" disabled={busy() || !storeId()} onClick={addVendor}>
                {t("channels.vendorAdd")}
              </Button>
            }
          >
            <p class="mb-3 text-sm text-ink-muted">{t("channels.vendorsHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-3">
                <Show
                  when={vendors().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("channels.vendorsEmpty")}</p>}
                >
                  <For each={vendors()}>
                    {(policy, index) => (
                      <div class="flex flex-wrap items-end gap-2 rounded-token border border-line bg-surface-raised p-3">
                        <TextField
                          label={t("channels.vendorName")}
                          value={policy.vendor}
                          onInput={(value) => updateVendor(index(), { vendor: value })}
                        />
                        <label class="block">
                          <span class="mb-1 block text-sm font-medium text-ink">
                            {t("channels.vendorAvailability")}
                          </span>
                          <select
                            class="min-h-touch rounded-token border border-line bg-surface-raised px-3 text-base text-ink"
                            value={policy.availability}
                            onChange={(event) =>
                              updateVendor(index(), {
                                availability: event.currentTarget.value as VendorAvailability,
                              })
                            }
                          >
                            <For each={VENDOR_AVAILABILITIES}>
                              {(value) => <option value={value}>{t(AVAILABILITY_LABEL[value])}</option>}
                            </For>
                          </select>
                        </label>
                        <TextField
                          label={t("channels.vendorPrepMinutes")}
                          type="number"
                          value={String(policy.prep_minutes)}
                          onInput={(value) => updateVendor(index(), { prep_minutes: Number(value) })}
                        />
                        <label class="flex items-center gap-2 text-sm text-ink">
                          <input
                            type="checkbox"
                            class="h-4 w-4"
                            checked={policy.enabled}
                            onChange={(event) =>
                              updateVendor(index(), { enabled: event.currentTarget.checked })
                            }
                          />
                          {t("channels.vendorEnabled")}
                        </label>
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() =>
                            setVendors((prev) => prev.filter((_, i) => i !== index()))
                          }
                        >
                          {t("channels.vendorRemove")}
                        </Button>
                      </div>
                    )}
                  </For>
                </Show>
                <div>
                  <Button disabled={busy()} onClick={() => void publishVendors()}>
                    {t("channels.publishVendors")}
                  </Button>
                </div>
              </div>
            ))}
          </Card>
        </div>
      </RequireContext>
    </div>
  );
}
