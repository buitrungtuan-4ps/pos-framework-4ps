import { For, Show, createEffect, createMemo } from "solid-js";

import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { useDarkTakeover } from "../lib/screen";
import {
  bump,
  firedLines,
  queueNumberFor,
  refreshQueueNumbers,
  type KitchenLine,
} from "../state/store";

interface TableGroup {
  orderId: string;
  // The table's published label, or "" for a counter order.
  label: string;
  lines: KitchenLine[];
}

// The pass: fired lines gathered by table, so the expeditor runs a whole table together. "All away"
// bumps the whole table's lines — the same durable `kitchen.ticket.bumped` event a KDS records (#44),
// so the pass and every kitchen screen agree the table is done rather than each holding a private
// acknowledgement. One table has one open order, so a group's lines share an order id.
export function Expo() {
  useDarkTakeover();

  // Grouped by order, which on a table is the table's one open order — and which keeps two counter
  // orders apart: they share the empty label, and grouping on it would have bumped one order's food
  // under the other's id.
  const groups = createMemo<TableGroup[]>(() => {
    const byOrder = new Map<string, TableGroup>();
    for (const line of firedLines()) {
      const group = byOrder.get(line.orderId) ?? {
        orderId: line.orderId,
        label: line.tableLabel,
        lines: [],
      };
      group.lines.push(line);
      byOrder.set(line.orderId, group);
    }
    return [...byOrder.values()].sort(
      (a, b) =>
        a.label.localeCompare(b.label, undefined, { numeric: true }) ||
        a.orderId.localeCompare(b.orderId),
    );
  });

  // A counter order is called by the guest's number, as on the kitchen board (see `Kds.tsx`).
  createEffect(() => {
    const counter = groups()
      .filter((group) => group.label === "")
      .map((group) => group.orderId);
    if (counter.some((orderId) => queueNumberFor(orderId) === undefined)) {
      void refreshQueueNumbers(counter);
    }
  });
  const counterLabel = (orderId: string) => {
    const number = queueNumberFor(orderId);
    return number === undefined
      ? t("kds.counter_order", { ref: orderId.slice(-4) })
      : t("counter.queue_number", { number });
  };

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
      <div class="grid grid-cols-1 gap-3 tablet:grid-cols-2 terminal:grid-cols-3">
        <For
          each={groups()}
          fallback={
            <p class="text-ink-muted" data-outcome="pass-clear">
              {t("expo.empty")}
            </p>
          }
        >
          {(group) => (
            <div class="rounded-token border border-line bg-surface-raised p-4">
              <p class="text-lg font-semibold">
                {group.label === ""
                  ? counterLabel(group.orderId)
                  : t("common.table", { label: group.label })}
              </p>
              <ul class="mt-2 flex flex-col gap-1">
                <For each={group.lines}>
                  {(line) => (
                    <li>
                      {line.name}
                      {/* The choices, on the same row and dimmed. The pass is checking a plated
                          table against what was ordered, and "Margherita" alone cannot be checked
                          against a 30cm. One row rather than the board's list, because the expeditor
                          is counting dishes and a second line per dish would halve how many fit. */}
                      <Show when={line.modifiers.length > 0}>
                        <span class="text-ink-muted" data-outcome="pass-modifiers">
                          {" "}
                          · {line.modifiers.join(" · ")}
                        </span>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
              <button
                type="button"
                class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
                data-step="runAway"
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
