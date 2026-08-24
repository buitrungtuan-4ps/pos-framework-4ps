// The daily rollup for the store in context (ADR-0060's admin read over the ADR-0022 rollup). Counts
// only — the rollup is event totals per trading day, never revenue or PII. The operator sets the
// tenant/store in the top bar, then loads; a store with nothing ingested yet reads as empty.

import { createSignal, For, Show } from "solid-js";

import { api, ApiError } from "../api/client";
import type { DailyRollup } from "../api/types";
import { t } from "../i18n";
import { formatCount } from "../lib/format";
import { storeId, tenantId } from "../state/session";
import { Banner, Button, Card, PageHeader } from "../components/ui";

export function Reports() {
  const [rows, setRows] = createSignal<DailyRollup[] | null>(null);
  const [error, setError] = createSignal("");
  const [busy, setBusy] = createSignal(false);

  const load = async () => {
    setError("");
    setBusy(true);
    try {
      setRows(await api.dailyRollups(tenantId(), storeId()));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : String(caught));
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

  return (
    <div>
      <PageHeader title={t("reports.title")} description={t("reports.description")} />
      <Show
        when={tenantId() && storeId()}
        fallback={<Banner tone="danger" message={t("context.required")} />}
      >
        <Card
          title={t("reports.daily")}
          actions={
            <Button variant="secondary" disabled={busy()} onClick={() => void load()}>
              {t("action.load")}
            </Button>
          }
        >
          <Show when={error()}>{(message) => <Banner tone="danger" message={message()} />}</Show>
          <Show when={rows()}>
            {(loaded) => (
              <Show
                when={loaded().length > 0}
                fallback={<p class="text-sm text-ink-muted">{t("reports.empty")}</p>}
              >
                <div class="overflow-x-auto">
                  <table class="w-full text-left text-sm">
                    <thead>
                      <tr class="border-b border-line text-ink-muted">
                        <th class="py-2 pr-4 font-medium">{t("reports.date")}</th>
                        <th class="py-2 pr-4 font-medium">{t("reports.events")}</th>
                        <th class="py-2 font-medium">{t("reports.byType")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={loaded()}>
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
            )}
          </Show>
        </Card>
      </Show>
    </div>
  );
}
