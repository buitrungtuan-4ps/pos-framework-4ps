// The Channels & payments screen (ADR-0080, Track M7). Four store-scoped settings, each published to
// one store's config node: which sales channels it accepts (`channels`), which payment methods
// (`tender`), its QR ordering guardrails (`qr`), and its per-marketplace vendor policies (`vendors`).
// Each is opt-in and never-blank — an unpublished node means "no restriction" — so publishing needs a
// store chosen in the top bar. The edge applies channels/tender as gates and qr as its staff-confirm
// source; the live marketplace loop for vendor policy is a flagged follow-up.
//
// The fifth card is `origins` (ADR-0111): which other origins that store's edge answers. It sits here
// rather than on Store settings because it is the same publish-a-node-to-one-store shape as the four
// above, and because it is a *channel* question in every sense that matters — a native shell or a
// second front-end is a way orders reach the box. The serving origin is never listed and never needs
// to be: the edge compares it against the request's own Host, so a store that publishes nothing keeps
// serving its own UI.

import { createEffect, createSignal, For, Show } from "solid-js";

import { api } from "../api/client";
import {
  SALES_CHANNELS,
  VENDOR_AVAILABILITIES,
  type QrGuardrails,
  type SalesChannel,
  type VendorAvailability,
  type VendorPolicy,
} from "../api/types";
import { type MessageKey, t } from "../i18n";
import { describePublish } from "../lib/publish-copy";
import { usePublishedNodes } from "../lib/published";
import { createAdminResource, failureOf } from "../lib/resource";
import { RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import {
  Banner,
  Button,
  Card,
  CheckboxField,
  PageHeader,
  SelectField,
  TextField,
} from "../components/ui";
import { PublishBar } from "../components/kit";
import { toast } from "../components/Toast";
import { apiMessage } from "../lib/errors";

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

/**
 * The most origins a store may publish, mirroring `pos_proto::origins::MAX_ORIGINS`.
 *
 * Enforced here only to stop an operator building a list the server will refuse whole; the server
 * refuses over-long and malformed lists regardless, and its refusal is the authority.
 */
const MAX_ORIGINS = 8;

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
  const [origins, setOrigins] = createSignal<string[]>([]);

  // The five store-scoped nodes this screen authors, read together.
  //
  // Without a store there is nothing to read: the nodes are per-shop, so the read answers `null`
  // for each rather than refusing, and the forms below fall back to their defaults. That is a fact
  // about the context, not a failure, and the store gate already says so on screen.
  const nodes = createAdminResource(
    async (tenant, store) => {
      if (!store) {
        return null;
      }
      const [channels, tender, qr, vendors, origins] = await Promise.all([
        api.readChannels(tenant, store),
        api.readTender(tenant, store),
        api.readQrGuardrails(tenant, store),
        api.readVendorPolicies(tenant, store),
        api.readOrigins(tenant, store),
      ]);
      return { channels, tender, qr, vendors, origins };
    },
    { scope: "tenant" },
  );
  const published = usePublishedNodes();

  const fail = (caught: unknown) => {
    const message = apiMessage(caught);
    setError(message);
    toast.error(message);
  };

  // The editable forms, primed from the read.
  //
  // Two states, not one: the resource holds what the store is running, these hold what the operator
  // has typed over it. An absent node means "no restriction" for four of the five — show every
  // channel and method enabled — so the fallback is the permissive default, which is what a store
  // that has never published one of these actually behaves like.
  createEffect(() => {
    const read = nodes.value();
    if (read === null) {
      setChannels([...SALES_CHANNELS]);
      setTender([...PAYMENT_METHODS]);
      setQr({ ...QR_DEFAULTS });
      setVendors([]);
      setOrigins([]);
      return;
    }
    setChannels(read.channels ? [...read.channels.enabled] : [...SALES_CHANNELS]);
    setTender(read.tender ? [...read.tender.accepted] : [...PAYMENT_METHODS]);
    const guardrails = read.qr ?? { ...QR_DEFAULTS };
    setQr(guardrails);
    setQrHoursOn(Boolean(guardrails.business_hours));
    setQrOpen(String(guardrails.business_hours?.open_hour ?? 0));
    setQrClose(String(guardrails.business_hours?.close_hour ?? 0));
    setQrOffset(String(guardrails.business_hours?.tz_offset_minutes ?? 0));
    setVendors(read.vendors ? read.vendors.policies.map((policy) => ({ ...policy })) : []);
    // An absent node is *not* "no restriction" here, unlike the four above: it means same-origin
    // only, which is how every store behaved before ADR-0111. So the empty list is the truth, and
    // the card must not pre-fill anything an operator did not publish.
    setOrigins(read.origins ? [...read.origins.allowed] : []);
  });


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
      await published.refresh();
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
      await published.refresh();
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
      await published.refresh();
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
      await published.refresh();
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

  const publishOrigins = async () => {
    // Trimmed and de-blanked before the round trip: a row an operator left empty is a row they
    // abandoned, not an origin. Everything else — wildcards, `null`, a pasted URL with a path — is
    // the server's to refuse, against the very rule the edge applies, so the message the operator
    // reads is the one the edge would have logged.
    const allowed = origins()
      .map((origin) => origin.trim())
      .filter((origin) => origin.length > 0);
    setBusy(true);
    try {
      await api.publishOrigins(tenantId(), storeId(), allowed);
      setOrigins(allowed);
      toast.ok(t("channels.originsPublished", { store: storeName() }));
      await published.refresh();
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

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
          {/* Two failures, two banners: the read's refusal is not the operator's to correct. */}
      <Show when={failureOf(nodes)}>
        {(message) => <Banner tone="danger" message={message()} />}
      </Show>
      <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          {/* Channels */}
          <Card title={t("channels.channelsTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("channels.channelsHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-4">
                <div class="flex flex-wrap gap-3">
                  <For each={SALES_CHANNELS}>
                    {(channel) => (
                      <CheckboxField
                        label={t(CHANNEL_LABEL[channel])}
                        checked={channels().includes(channel)}
                        onChange={() => toggleChannel(channel)}
                      />
                    )}
                  </For>
                </div>
                <PublishBar
                  label={t("channels.publishTo", { store: storeName() })}
                  publishedAtMs={published.publishedAtMs("channels")}
                  describe={describePublish}
                  publishLabel={t("channels.publishChannels")}
                  busy={busy()}
                  onPublish={() => void publishChannels()}
                />
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
                      <CheckboxField
                        label={t(TENDER_LABEL[method] ?? "channels.tender.other")}
                        checked={tender().includes(method)}
                        onChange={() => toggleTender(method)}
                      />
                    )}
                  </For>
                </div>
                <PublishBar
                  label={t("channels.publishTo", { store: storeName() })}
                  publishedAtMs={published.publishedAtMs("tender")}
                  describe={describePublish}
                  publishLabel={t("channels.publishTender")}
                  busy={busy()}
                  onPublish={() => void publishTender()}
                />
              </div>
            ))}
          </Card>

          {/* QR guardrails */}
          <Card title={t("channels.qrTitle")}>
            <p class="mb-3 text-sm text-ink-muted">{t("channels.qrHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-3">
                <CheckboxField
                  label={t("channels.qrEnabled")}
                  checked={qr().enabled}
                  onChange={(on) => setQr({ ...qr(), enabled: on })}
                />
                <CheckboxField
                  label={t("channels.qrStaffConfirm")}
                  checked={qr().staff_confirmation_required}
                  onChange={(on) => setQr({ ...qr(), staff_confirmation_required: on })}
                />
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
                <CheckboxField
                  label={t("channels.qrHoursOn")}
                  checked={qrHoursOn()}
                  onChange={setQrHoursOn}
                />
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
                <PublishBar
                  label={t("channels.publishTo", { store: storeName() })}
                  publishedAtMs={published.publishedAtMs("qr")}
                  describe={describePublish}
                  publishLabel={t("channels.publishQr")}
                  busy={busy()}
                  onPublish={() => void publishQr()}
                />
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
                        <SelectField
                          label={t("channels.vendorAvailability")}
                          value={policy.availability}
                          options={VENDOR_AVAILABILITIES.map((value) => ({
                            value,
                            label: t(AVAILABILITY_LABEL[value]),
                          }))}
                          onChange={(value) =>
                            updateVendor(index(), {
                              availability: value as VendorAvailability,
                            })
                          }
                        />
                        <TextField
                          label={t("channels.vendorPrepMinutes")}
                          type="number"
                          value={String(policy.prep_minutes)}
                          onInput={(value) => updateVendor(index(), { prep_minutes: Number(value) })}
                        />
                        <CheckboxField
                          label={t("channels.vendorEnabled")}
                          checked={policy.enabled}
                          onChange={(on) => updateVendor(index(), { enabled: on })}
                        />
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
                <PublishBar
                  label={t("channels.publishTo", { store: storeName() })}
                  publishedAtMs={published.publishedAtMs("vendors")}
                  describe={describePublish}
                  publishLabel={t("channels.publishVendors")}
                  busy={busy()}
                  onPublish={() => void publishVendors()}
                />
              </div>
            ))}
          </Card>

          {/* Origins (ADR-0111) */}
          <Card
            title={t("channels.originsTitle")}
            actions={
              <Button
                variant="secondary"
                disabled={busy() || !storeId() || origins().length >= MAX_ORIGINS}
                onClick={() => setOrigins((prev) => [...prev, ""])}
              >
                {t("channels.originAdd")}
              </Button>
            }
          >
            <p class="mb-3 text-sm text-ink-muted">{t("channels.originsHint")}</p>
            {storeGate(() => (
              <div class="flex flex-col gap-3">
                <Show
                  when={origins().length > 0}
                  fallback={<p class="text-sm text-ink-muted">{t("channels.originsEmpty")}</p>}
                >
                  <For each={origins()}>
                    {(origin, index) => (
                      <div class="flex flex-wrap items-end gap-2 rounded-token border border-line bg-surface-raised p-3">
                        <TextField
                          label={t("channels.originValue")}
                          value={origin}
                          placeholder={t("channels.originPlaceholder")}
                          onInput={(value) =>
                            setOrigins((prev) =>
                              prev.map((held, i) => (i === index() ? value : held)),
                            )
                          }
                        />
                        <Button
                          variant="secondary"
                          disabled={busy()}
                          onClick={() =>
                            setOrigins((prev) => prev.filter((_, i) => i !== index()))
                          }
                        >
                          {t("channels.originRemove")}
                        </Button>
                      </div>
                    )}
                  </For>
                </Show>
                {/* Publishing an empty list is deliberate and allowed: it withdraws every second
                    origin, which is distinct from never having published one. */}
                <PublishBar
                  label={t("channels.publishTo", { store: storeName() })}
                  publishedAtMs={published.publishedAtMs("origins")}
                  describe={describePublish}
                  publishLabel={t("channels.publishOrigins")}
                  busy={busy()}
                  onPublish={() => void publishOrigins()}
                />
              </div>
            ))}
          </Card>
        </div>
      </RequireContext>
    </div>
  );
}
