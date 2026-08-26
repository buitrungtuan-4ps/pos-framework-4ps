import { For, Show, createMemo } from "solid-js";

import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { useDarkTakeover } from "../lib/screen";
import { bump, firedLines, type KitchenLine } from "../state/store";

interface TableGroup {
  label: string;
  lines: KitchenLine[];
}

// The pass: fired lines gathered by table, so the expeditor runs a whole table together. "All away"
// bumps the whole table's lines — the same durable `kitchen.ticket.bumped` event a KDS records (#44),
// so the pass and every kitchen screen agree the table is done rather than each holding a private
// acknowledgement. One table has one open order, so a group's lines share an order id.
export function Expo() {
  useDarkTakeover();

  const groups = createMemo<TableGroup[]>(() => {
    const byTable = new Map<string, KitchenLine[]>();
    for (const line of firedLines()) {
      const lines = byTable.get(line.tableLabel) ?? [];
      lines.push(line);
      byTable.set(line.tableLabel, lines);
    }
    return [...byTable.entries()]
      .map(([label, lines]) => ({ label, lines }))
      .sort((a, b) => a.label.localeCompare(b.label, undefined, { numeric: true }));
  });

  const runAway = (lines: KitchenLine[]) => {
    const [first] = lines;
    if (first === undefined) {
      return;
    }
    void bump(
      first.orderId,
      lines.map((line) => line.orderLineId),
    );
  };

  return (
    <section class="p-4">
      <PageHeader title={t("expo.title")} size="xl" />
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
        <For each={groups()} fallback={<p class="text-ink-muted">{t("expo.empty")}</p>}>
          {(group) => (
            <div class="rounded-token border border-line bg-surface-raised p-4">
              <p class="text-lg font-semibold">{t("common.table", { label: group.label })}</p>
              <ul class="mt-2 flex flex-col gap-1">
                <For each={group.lines}>{(line) => <li>{line.name}</li>}</For>
              </ul>
              <button
                type="button"
                class="mt-3 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink"
                onClick={() => runAway(group.lines)}
              >
                {t("expo.all_away")}
              </button>
            </div>
          )}
        </For>
      </div>
      <Show when={groups().length > 0}>
        <p class="mt-4 text-sm text-ink-muted">{t("expo.count", { count: groups().length })}</p>
      </Show>
    </section>
  );
}
