// The tax-rate editor (ADR-0074, Track M4): a tenant's per-(tax class × sales channel) rate table,
// edited as a grid — rows are the tenant's tax classes, columns the sales channels, each cell a rate
// in percent. A blank cell means the class is not priced on that channel (the edge refuses such a
// sale rather than charging no tax, so a blank is a deliberate "not sold here", not "0%"). Save writes
// the whole table; Publish pushes it to the store in context as the `tax` config node the edge
// applies. Rates are tenant-level (authored once); publishing is per-store, so it needs a store in the
// top-bar context.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import { SALES_CHANNELS, type SalesChannel, type TaxClass, type TaxRate } from "../api/types";
import { type MessageKey, t } from "../i18n";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { storeId, storeName, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";
import { EmptyState } from "../components/kit";
import { toast } from "../components/Toast";

/** The already-defined per-channel labels (shared with the Catalog price editor). */
const CHANNEL_LABEL: Record<SalesChannel, MessageKey> = {
  SALES_CHANNEL_DINE_IN: "channel.dineIn",
  SALES_CHANNEL_TAKEAWAY: "channel.takeaway",
  SALES_CHANNEL_DELIVERY: "channel.delivery",
  SALES_CHANNEL_QR: "channel.qr",
  SALES_CHANNEL_API: "channel.api",
};

/** The highest rate the server accepts, in basis points (100%). */
const MAX_BPS = 10_000;

/** A grid cell's key: the tax class and the channel it prices. */
function cellKey(taxClassId: string, channel: SalesChannel): string {
  return `${taxClassId}|${channel}`;
}

/** A basis-points rate as a percent string for the input (`1000` → `"10"`, `850` → `"8.5"`). */
function bpsToPercent(bps: number): string {
  return String(bps / 100);
}

/** A percent input as basis points, or `null` when blank or not a number in `[0, 100]`. */
function percentToBps(text: string): number | null {
  const trimmed = text.trim();
  if (trimmed === "") {
    return null;
  }
  const percent = Number(trimmed);
  if (!Number.isFinite(percent) || percent < 0) {
    return null;
  }
  const bps = Math.round(percent * 100);
  return bps > MAX_BPS ? null : bps;
}

export function TaxRates() {
  const [classes, setClasses] = createSignal<TaxClass[]>([]);
  // The edited grid: cellKey → the percent text in the input (empty string means "not priced").
  const [cells, setCells] = createSignal<Record<string, string>>({});
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
    toast.error(message);
  };

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      const [taxClasses, rates] = await Promise.all([
        api.listTaxClasses(tenantId()),
        api.listTaxRates(tenantId()),
      ]);
      setClasses(taxClasses.filter((row) => row.status === "active"));
      const grid: Record<string, string> = {};
      for (const rate of rates) {
        grid[cellKey(rate.tax_class_id, rate.sales_channel)] = bpsToPercent(rate.rate_bps);
      }
      setCells(grid);
      setLoaded(true);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const setCell = (taxClassId: string, channel: SalesChannel, value: string) => {
    setCells({ ...cells(), [cellKey(taxClassId, channel)]: value });
  };

  // Every non-blank, in-range cell becomes a rate row; a blank or invalid cell is simply absent.
  const rows = (): TaxRate[] => {
    const out: TaxRate[] = [];
    for (const taxClass of classes()) {
      for (const channel of SALES_CHANNELS) {
        const bps = percentToBps(cells()[cellKey(taxClass.tax_class_id, channel)] ?? "");
        if (bps !== null) {
          out.push({
            tax_class_id: taxClass.tax_class_id,
            sales_channel: channel,
            rate_bps: bps,
          });
        }
      }
    }
    return out;
  };

  const save = async () => {
    setError("");
    setBusy(true);
    try {
      await api.setTaxRates(tenantId(), rows());
      toast.ok(t("taxRates.saved"));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const publish = async () => {
    setError("");
    setBusy(true);
    try {
      await api.publishTax(tenantId(), storeId());
      toast.ok(t("taxRates.published", { store: storeName() }));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <PageHeader title={t("taxRates.title")} description={t("taxRates.description")} />
      <RequireContext need="tenant">
        <Card
          title={t("taxRates.grid")}
          actions={
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.refresh")}
            </Button>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={loaded()}>
            <div class="mb-4 overflow-x-auto">
              <Show
                when={classes().length > 0}
                fallback={
                  <EmptyState
                    title={t("taxRates.noClasses")}
                    description={t("taxRates.noClassesHint")}
                  />
                }
              >
                <table class="w-full text-left text-sm">
                  <thead>
                    <tr class="border-b border-line text-ink-muted">
                      <th class="py-2 pr-4 font-medium">{t("taxRates.taxClass")}</th>
                      <For each={SALES_CHANNELS}>
                        {(channel) => (
                          <th class="py-2 pr-4 font-medium">{t(CHANNEL_LABEL[channel])}</th>
                        )}
                      </For>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={classes()}>
                      {(taxClass) => (
                        <tr class="border-b border-line">
                          <td class="py-2 pr-4 align-middle text-ink">{taxClass.name}</td>
                          <For each={SALES_CHANNELS}>
                            {(channel) => (
                              <td class="py-2 pr-4">
                                <div class="flex items-center gap-1">
                                  <input
                                    type="text"
                                    inputmode="decimal"
                                    class="min-h-touch w-20 rounded-token border border-line bg-surface-raised px-2 text-sm text-ink"
                                    aria-label={`${taxClass.name} ${t(CHANNEL_LABEL[channel])}`}
                                    value={cells()[cellKey(taxClass.tax_class_id, channel)] ?? ""}
                                    onInput={(event) =>
                                      setCell(taxClass.tax_class_id, channel, event.currentTarget.value)
                                    }
                                  />
                                  <span aria-hidden="true" class="text-ink-muted">
                                    %
                                  </span>
                                </div>
                              </td>
                            )}
                          </For>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </Show>
            </div>
            <p class="mb-3 text-sm text-ink-muted">{t("taxRates.blankHint")}</p>
            <div class="flex flex-wrap items-center gap-3">
              <Button disabled={busy()} onClick={() => void save()}>
                {t("action.save")}
              </Button>
              <Button
                variant="secondary"
                disabled={busy() || !storeId()}
                onClick={() => void publish()}
              >
                {t("taxRates.publish")}
              </Button>
              <span class="text-sm text-ink-muted">
                <Show when={storeId()} fallback={t("taxRates.publishNeedsStore")}>
                  {t("taxRates.publishTo", { store: storeName() })}
                </Show>
              </span>
            </div>
          </Show>
        </Card>
      </RequireContext>
    </div>
  );
}
