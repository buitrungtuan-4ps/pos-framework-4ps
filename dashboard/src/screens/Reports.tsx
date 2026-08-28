// Reports & analytics for the store in context (ADR-0081, Track O4). A windowed view over the
// materialised rollups: activity counts (everyone), and — for Owner/Admin, because prices are T2 —
// revenue, product mix, an X/Z report, and a cross-store comparison. Charts are hand-rolled inline
// SVG (no chart library, so nothing to load past the CSP). The operator sets the tenant/store in the
// top bar; the date-range window defaults to the server's most recent 90 trading days.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { DailyRevenue, DailyRollup, Store, XzReport } from "../api/types";
import { t } from "../i18n";
import { formatCount, formatMoney } from "../lib/format";
import { onScopedContext, RequireContext } from "../lib/scoped";
import { actingAdmin, storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader, TextField } from "../components/ui";

/** Owner/Admin see money (revenue is T2); the server re-checks, so this only hides what would 403. */
function canReadRevenue(): boolean {
  const role = actingAdmin()?.role;
  return role === "owner" || role === "admin";
}

/** A minor-unit amount in a currency, formatted for the active locale (reuses the Money formatter). */
function money(minor: number, currency: string): string {
  return formatMoney({ amount_minor: minor, currency_code: currency || "" });
}

/** A minimal, theme-aware inline-SVG bar chart. The data table beside it carries the real values, so
 * the chart is decorative — labelled for a screen reader, but not the source of truth. */
function BarChart(props: { data: number[]; label: string }) {
  const max = () => Math.max(1, ...props.data);
  const count = () => Math.max(1, props.data.length);
  return (
    <Show
      when={props.data.length > 0}
      fallback={<p class="text-sm text-ink-muted">{t("reports.noChart")}</p>}
    >
      <svg
        viewBox="0 0 100 100"
        preserveAspectRatio="none"
        class="h-28 w-full text-accent"
        role="img"
        aria-label={props.label}
      >
        <For each={props.data}>
          {(value, index) => {
            const width = 100 / count();
            const height = (value / max()) * 100;
            return (
              <rect
                x={index() * width + width * 0.1}
                y={100 - height}
                width={width * 0.8}
                height={height}
                fill="currentColor"
              />
            );
          }}
        </For>
      </svg>
    </Show>
  );
}

export function Reports() {
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [from, setFrom] = createSignal("");
  const [to, setTo] = createSignal("");

  const [rows, setRows] = createSignal<DailyRollup[]>([]);
  const [revenue, setRevenue] = createSignal<DailyRevenue[]>([]);
  const [xz, setXz] = createSignal<XzReport | null>(null);
  const [xzDate, setXzDate] = createSignal("");
  const [cross, setCross] = createSignal<{ name: string; net: number; currency: string }[] | null>(
    null,
  );

  const win = () => ({ from: from() || undefined, to: to() || undefined });

  const fail = (caught: unknown) => {
    const message = caught instanceof ApiError ? caught.message : String(caught);
    setError(message);
  };

  const load = async () => {
    if (!storeId()) {
      return;
    }
    setError("");
    setBusy(true);
    try {
      const jobs: Promise<unknown>[] = [
        api.dailyRollups(tenantId(), storeId(), win()).then(setRows),
      ];
      if (canReadRevenue()) {
        jobs.push(api.dailyRevenue(tenantId(), storeId(), win()).then(setRevenue));
        jobs.push(api.xzReport(tenantId(), storeId(), xzDate() || undefined).then(setXz));
      }
      await Promise.all(jobs);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Load on open and whenever the tenant/store changes — never with an empty context (F0).
  onScopedContext("store", () => void load());

  const loadXz = async () => {
    setBusy(true);
    try {
      setXz(await api.xzReport(tenantId(), storeId(), xzDate() || undefined));
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  // Cross-store: for each of the tenant's active stores, its total net over the window. N reads, run
  // on demand (not on every load) since a tenant can have many stores.
  const loadCrossStore = async () => {
    setBusy(true);
    try {
      const stores = (await api.listStores(tenantId())).filter(
        (store: Store) => store.status === "active",
      );
      const totals = await Promise.all(
        stores.map(async (store) => {
          const days = await api.dailyRevenue(tenantId(), store.store_id, win());
          const net = days.reduce((sum, day) => sum + day.net, 0);
          const currency = days.find((day) => day.currency_code)?.currency_code ?? "";
          return { name: store.name, net, currency };
        }),
      );
      totals.sort((a, b) => b.net - a.net);
      setCross(totals);
    } catch (caught) {
      fail(caught);
    } finally {
      setBusy(false);
    }
  };

  const topTypes = (row: DailyRollup) =>
    Object.entries(row.by_type)
      .sort(([, a], [, b]) => b - a)
      .slice(0, 3)
      .map(([type, count]) => `${type} ${formatCount(count)}`)
      .join(", ");

  // Aggregate the product mix across the window: item id → {name, value}, top 10 by ordered value.
  const productMix = () => {
    const totals = new Map<string, { name: string; value: number }>();
    for (const day of revenue()) {
      for (const [id, mix] of Object.entries(day.by_item)) {
        const current = totals.get(id) ?? { name: mix.name, value: 0 };
        current.name = mix.name || current.name;
        current.value += mix.ordered_value;
        totals.set(id, current);
      }
    }
    return [...totals.values()].sort((a, b) => b.value - a.value).slice(0, 10);
  };

  const revenueCurrency = () => revenue().find((day) => day.currency_code)?.currency_code ?? "";

  return (
    <div>
      <PageHeader title={t("reports.title")} description={t("reports.description")} />
      <RequireContext need="store">
        <div class="flex flex-col gap-6">
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>

          {/* Window */}
          <Card title={t("reports.window")}>
            <div class="flex flex-wrap items-end gap-4">
              <TextField label={t("reports.from")} type="date" value={from()} onInput={setFrom} />
              <TextField label={t("reports.to")} type="date" value={to()} onInput={setTo} />
              <Button disabled={busy()} onClick={() => void load()}>
                {t("action.refresh")}
              </Button>
            </div>
            <p class="mt-2 text-sm text-ink-muted">{t("reports.windowHint")}</p>
          </Card>

          {/* Activity */}
          <Card
            title={t("reports.activityTitle")}
            actions={
              <Button
                variant="secondary"
                disabled={busy()}
                onClick={() => void api.exportRollupsCsv(tenantId(), storeId(), win())}
              >
                {t("reports.exportCsv")}
              </Button>
            }
          >
            <Show
              when={rows().length > 0}
              fallback={<p class="text-sm text-ink-muted">{t("reports.empty")}</p>}
            >
              <BarChart data={rows().map((row) => row.total_events)} label={t("reports.activityChart")} />
              <div class="mt-3 overflow-x-auto">
                <table class="w-full text-left text-sm">
                  <thead>
                    <tr class="border-b border-line text-ink-muted">
                      <th class="py-2 pr-4 font-medium">{t("reports.date")}</th>
                      <th class="py-2 pr-4 font-medium">{t("reports.events")}</th>
                      <th class="py-2 font-medium">{t("reports.byType")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    <For each={rows()}>
                      {(row) => (
                        <tr class="border-b border-line text-ink">
                          <td class="py-2 pr-4 font-mono">{row.business_date}</td>
                          <td class="py-2 pr-4">{formatCount(row.total_events)}</td>
                          <td class="py-2 text-ink-muted">{topTypes(row)}</td>
                        </tr>
                      )}
                    </For>
                  </tbody>
                </table>
              </div>
            </Show>
          </Card>

          <Show
            when={canReadRevenue()}
            fallback={
              <Card title={t("reports.revenueTitle")}>
                <p class="text-sm text-ink-muted">{t("reports.needsRevenue")}</p>
              </Card>
            }
          >
            {/* Revenue */}
            <Card
              title={t("reports.revenueTitle")}
              actions={
                <Button
                  variant="secondary"
                  disabled={busy()}
                  onClick={() => void api.exportRevenueCsv(tenantId(), storeId(), win())}
                >
                  {t("reports.exportCsv")}
                </Button>
              }
            >
              <Show
                when={revenue().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("reports.revenueEmpty")}</p>}
              >
                <BarChart data={revenue().map((day) => day.net)} label={t("reports.revenueChart")} />
                <div class="mt-3 overflow-x-auto">
                  <table class="w-full text-left text-sm">
                    <thead>
                      <tr class="border-b border-line text-ink-muted">
                        <th class="py-2 pr-4 font-medium">{t("reports.date")}</th>
                        <th class="py-2 pr-4 font-medium">{t("reports.bills")}</th>
                        <th class="py-2 pr-4 font-medium">{t("reports.tax")}</th>
                        <th class="py-2 font-medium">{t("reports.net")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={revenue()}>
                        {(day) => (
                          <tr class="border-b border-line text-ink">
                            <td class="py-2 pr-4 font-mono">{day.business_date}</td>
                            <td class="py-2 pr-4">{formatCount(day.bills)}</td>
                            <td class="py-2 pr-4">{money(day.tax, day.currency_code)}</td>
                            <td class="py-2 font-medium">{money(day.net, day.currency_code)}</td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>
            </Card>

            {/* Product mix */}
            <Card title={t("reports.productMixTitle")}>
              <Show
                when={productMix().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("reports.productMixEmpty")}</p>}
              >
                <div class="flex flex-col gap-2">
                  <For each={productMix()}>
                    {(item) => {
                      const max = productMix()[0]?.value || 1;
                      return (
                        <div class="flex items-center gap-3">
                          <span class="w-40 shrink-0 truncate text-sm text-ink">{item.name}</span>
                          <span class="h-3 flex-1 rounded-token bg-surface-raised">
                            <span
                              class="block h-3 rounded-token bg-accent"
                              style={{ width: `${Math.round((item.value / max) * 100)}%` }}
                            />
                          </span>
                          <span class="w-28 shrink-0 text-right text-sm text-ink-muted">
                            {money(item.value, revenueCurrency())}
                          </span>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </Show>
            </Card>

            {/* X/Z report */}
            <Card title={t("reports.xzTitle")}>
              <p class="mb-3 text-sm text-ink-muted">{t("reports.xzHint")}</p>
              <div class="mb-4 flex flex-wrap items-end gap-4">
                <TextField
                  label={t("reports.xzDate")}
                  type="date"
                  value={xzDate()}
                  onInput={setXzDate}
                />
                <Button variant="secondary" disabled={busy()} onClick={() => void loadXz()}>
                  {t("reports.xzView")}
                </Button>
              </div>
              <Show when={xz()} fallback={<p class="text-sm text-ink-muted">{t("reports.empty")}</p>}>
                {(report) => (
                  <dl class="grid grid-cols-2 gap-x-6 gap-y-2 text-sm sm:grid-cols-3">
                    <div>
                      <dt class="text-ink-muted">{t("reports.xzKind")}</dt>
                      <dd class="font-medium text-ink">
                        {report().kind === "Z" ? t("reports.xzKindZ") : t("reports.xzKindX")}
                        {" · "}
                        <span class="font-mono">{report().business_date}</span>
                      </dd>
                    </div>
                    <div>
                      <dt class="text-ink-muted">{t("reports.net")}</dt>
                      <dd class="font-medium text-ink">
                        {money(report().revenue.net, report().revenue.currency_code)}
                      </dd>
                    </div>
                    <div>
                      <dt class="text-ink-muted">{t("reports.bills")}</dt>
                      <dd class="text-ink">{formatCount(report().revenue.bills)}</dd>
                    </div>
                    <div>
                      <dt class="text-ink-muted">{t("reports.xzExpected")}</dt>
                      <dd class="text-ink">
                        {money(report().cash.expected, report().cash.currency_code)}
                      </dd>
                    </div>
                    <div>
                      <dt class="text-ink-muted">{t("reports.xzCounted")}</dt>
                      <dd class="text-ink">
                        {money(report().cash.counted, report().cash.currency_code)}
                      </dd>
                    </div>
                    <div>
                      <dt class="text-ink-muted">{t("reports.xzVariance")}</dt>
                      <dd class="font-medium text-ink">
                        {money(report().cash.variance, report().cash.currency_code)}
                      </dd>
                    </div>
                  </dl>
                )}
              </Show>
            </Card>

            {/* Cross-store comparison */}
            <Card
              title={t("reports.crossStoreTitle")}
              actions={
                <Button variant="secondary" disabled={busy()} onClick={() => void loadCrossStore()}>
                  {t("reports.crossStoreLoad")}
                </Button>
              }
            >
              <p class="mb-3 text-sm text-ink-muted">{t("reports.crossStoreHint")}</p>
              <Show when={cross()}>
                {(stores) => (
                  <Show
                    when={stores().length > 0}
                    fallback={<p class="text-sm text-ink-muted">{t("reports.revenueEmpty")}</p>}
                  >
                    <div class="overflow-x-auto">
                      <table class="w-full text-left text-sm">
                        <thead>
                          <tr class="border-b border-line text-ink-muted">
                            <th class="py-2 pr-4 font-medium">{t("reports.crossStoreStore")}</th>
                            <th class="py-2 font-medium">{t("reports.crossStoreNet")}</th>
                          </tr>
                        </thead>
                        <tbody>
                          <For each={stores()}>
                            {(store) => (
                              <tr class="border-b border-line text-ink">
                                <td class="py-2 pr-4">{store.name}</td>
                                <td class="py-2 font-medium">{money(store.net, store.currency)}</td>
                              </tr>
                            )}
                          </For>
                        </tbody>
                      </table>
                    </div>
                  </Show>
                )}
              </Show>
            </Card>
          </Show>
        </div>
      </RequireContext>
    </div>
  );
}
