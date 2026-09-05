// The tax-rate editor (ADR-0074, Track M4): a tenant's per-(tax class × sales channel) rate table,
// edited as a grid — rows are the tenant's tax classes, columns the sales channels, each cell a rate
// in percent. A blank cell means the class is not priced on that channel (the edge refuses such a
// sale rather than charging no tax, so a blank is a deliberate "not sold here", not "0%"). Save writes
// the whole table; Publish pushes it to the store in context as the `tax` config node the edge
// applies. Rates are tenant-level (authored once); publishing is per-store, so it needs a store in the
// top-bar context.
//
// Each priced cell also takes a **breakdown** (ADR-0104): the named parts the invoice prints, typed
// as `CGST 2.5, SGST 2.5`. India needs it — an intra-state tax invoice must show the two halves
// separately, because they go to different governments — and most of the world leaves it blank,
// which means "print one line". The parts must sum to the cell's own rate; the screen says so
// inline, and the server refuses a save where they do not.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import {
  SALES_CHANNELS,
  type SalesChannel,
  type TaxClass,
  type TaxComponent,
  type TaxRate,
} from "../api/types";
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

/**
 * A breakdown as `CGST 2.5, SGST 2.5` into named parts, or `null` when any part is malformed
 * (ADR-0104).
 *
 * `null` rather than dropping the bad part, because a breakdown that silently loses a half would
 * print an invoice missing a tax the guest paid — worse than refusing to save. The name is
 * everything up to the last space so a two-word label survives; the rate is what follows it.
 */
function parseComponents(text: string): TaxComponent[] | null {
  const trimmed = text.trim();
  if (trimmed === "") {
    return [];
  }
  const parts: TaxComponent[] = [];
  for (const chunk of trimmed.split(",")) {
    const piece = chunk.trim();
    if (piece === "") {
      continue;
    }
    const split = piece.lastIndexOf(" ");
    if (split <= 0) {
      return null;
    }
    const name = piece.slice(0, split).trim();
    const bps = percentToBps(piece.slice(split + 1));
    if (name === "" || bps === null) {
      return null;
    }
    parts.push({ name, rate_bps: bps });
  }
  return parts;
}

/** Named parts back into the text the input shows. */
function formatComponents(components: readonly TaxComponent[]): string {
  return components
    .map((component) => `${component.name} ${bpsToPercent(component.rate_bps)}`)
    .join(", ");
}

export function TaxRates() {
  const [classes, setClasses] = createSignal<TaxClass[]>([]);
  // The edited grid: cellKey → the percent text in the input (empty string means "not priced").
  const [cells, setCells] = createSignal<Record<string, string>>({});
  // The breakdown beside each cell: cellKey → `CGST 2.5, SGST 2.5`. Empty means one printed line,
  // which is most of the world (ADR-0104).
  const [breakdowns, setBreakdowns] = createSignal<Record<string, string>>({});
  const [loaded, setLoaded] = createSignal(false);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  // The version the grid was read at, or `null` for a tenant that has never saved rates (ADR-0095).
  const [version, setVersion] = createSignal<string | null>(null);

  // A `412` means somebody else saved the grid while this one was open. The screen reloads rather
  // than offering a retry: retrying would re-apply the overwrite the refusal exists to prevent, and
  // the operator needs to see what actually changed before deciding again.
  const fail = async (caught: unknown) => {
    if (caught instanceof ApiError && caught.isStale) {
      const message = t("taxRates.stale");
      setError(message);
      toast.error(message);
      await load();
      return;
    }
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
      setVersion(rates.etag);
      const grid: Record<string, string> = {};
      const parts: Record<string, string> = {};
      for (const rate of rates.value) {
        const key = cellKey(rate.tax_class_id, rate.sales_channel);
        grid[key] = bpsToPercent(rate.rate_bps);
        parts[key] = formatComponents(rate.components ?? []);
      }
      setCells(grid);
      setBreakdowns(parts);
      setLoaded(true);
    } catch (caught) {
      await fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant changes — never with an empty context (F0).
  onScopedContext("tenant", () => void load());

  const setCell = (taxClassId: string, channel: SalesChannel, value: string) => {
    setCells({ ...cells(), [cellKey(taxClassId, channel)]: value });
  };

  const setBreakdown = (taxClassId: string, channel: SalesChannel, value: string) => {
    setBreakdowns({ ...breakdowns(), [cellKey(taxClassId, channel)]: value });
  };

  /**
   * Why a cell's breakdown cannot be saved, or `null` when it can.
   *
   * Shown beside the input rather than only on the save's refusal, because the operator typing
   * `CGST 2.5, SGST 2` should not have to press Save to find out — and because a breakdown that does
   * not add up is the one way this feature produces a document an auditor rejects.
   */
  const breakdownProblem = (taxClassId: string, channel: SalesChannel): MessageKey | null => {
    const key = cellKey(taxClassId, channel);
    const text = breakdowns()[key] ?? "";
    if (text.trim() === "") {
      return null;
    }
    const parts = parseComponents(text);
    if (parts === null) {
      return "taxRates.breakdownMalformed";
    }
    const rate = percentToBps(cells()[key] ?? "");
    if (rate === null) {
      return "taxRates.breakdownNeedsRate";
    }
    const total = parts.reduce((sum, part) => sum + part.rate_bps, 0);
    return total === rate ? null : "taxRates.breakdownUnbalanced";
  };

  /** Whether any cell's breakdown would be refused — the Save button's gate. */
  const anyBreakdownProblem = () =>
    classes().some((taxClass) =>
      SALES_CHANNELS.some(
        (channel) => breakdownProblem(taxClass.tax_class_id, channel) !== null,
      ),
    );

  // Every non-blank, in-range cell becomes a rate row; a blank or invalid cell is simply absent.
  const rows = (): TaxRate[] => {
    const out: TaxRate[] = [];
    for (const taxClass of classes()) {
      for (const channel of SALES_CHANNELS) {
        const key = cellKey(taxClass.tax_class_id, channel);
        const bps = percentToBps(cells()[key] ?? "");
        if (bps !== null) {
          out.push({
            tax_class_id: taxClass.tax_class_id,
            sales_channel: channel,
            rate_bps: bps,
            // A malformed breakdown never reaches the server: `anyBreakdownProblem` disables Save,
            // and this is the second line rather than a silent empty list.
            components: parseComponents(breakdowns()[key] ?? "") ?? [],
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
      await api.setTaxRates(tenantId(), rows(), version());
      // Re-read rather than trusting the save's own `ETag`: the next edit has to be made against
      // what is stored, and the reload is one request either way.
      await load();
      toast.ok(t("taxRates.saved"));
    } catch (caught) {
      await fail(caught);
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
      await fail(caught);
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
                                <input
                                  type="text"
                                  class="mt-1 min-h-touch w-40 rounded-token border border-line bg-surface-raised px-2 text-xs text-ink"
                                  placeholder={t("taxRates.breakdownPlaceholder")}
                                  aria-label={`${taxClass.name} ${t(CHANNEL_LABEL[channel])} ${t("taxRates.breakdown")}`}
                                  value={breakdowns()[cellKey(taxClass.tax_class_id, channel)] ?? ""}
                                  onInput={(event) =>
                                    setBreakdown(
                                      taxClass.tax_class_id,
                                      channel,
                                      event.currentTarget.value,
                                    )
                                  }
                                />
                                <Show when={breakdownProblem(taxClass.tax_class_id, channel)}>
                                  {(problem) => (
                                    <p class="mt-1 w-40 text-xs text-danger">{t(problem())}</p>
                                  )}
                                </Show>
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
            <p class="mb-1 text-sm text-ink-muted">{t("taxRates.blankHint")}</p>
            <p class="mb-3 text-sm text-ink-muted">{t("taxRates.breakdownHint")}</p>
            <div class="flex flex-wrap items-center gap-3">
              <Button disabled={busy() || anyBreakdownProblem()} onClick={() => void save()}>
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
