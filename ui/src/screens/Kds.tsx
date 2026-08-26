import { For, Show } from "solid-js";

import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { useDarkTakeover } from "../lib/screen";
import { bump, firedLines } from "../state/store";

// The kitchen display: every fired line, large, on a dark panel. Bumping a ticket records the durable
// `kitchen.ticket.bumped` event (#44) and fans it out, so a second KDS agrees the ticket is done
// rather than holding a private, divergent "done" flag — and a screen coming online after the bump
// reads the same prepared set. The bumped line drops off this screen at once; the store folds the
// returning event so it stays off.
export function Kds() {
  useDarkTakeover();
  const visible = () => firedLines();
  const onBump = (orderId: string, orderLineId: string) => {
    void bump(orderId, [orderLineId]);
  };

  return (
    <section class="p-4">
      <PageHeader title={t("kds.title")} size="xl" />
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
        <For each={visible()} fallback={<p class="text-ink-muted">{t("kds.empty")}</p>}>
          {(line) => (
            <button
              type="button"
              class="flex min-h-money flex-col items-start gap-1 rounded-token border border-line bg-surface-raised p-4 text-left"
              onClick={() => onBump(line.orderId, line.orderLineId)}
            >
              <span class="text-sm text-ink-muted">{t("common.table", { label: line.tableLabel })}</span>
              <span class="text-xl font-semibold">{line.name}</span>
              <span class="mt-1 text-sm text-ink-muted">{t("kds.bump")}</span>
            </button>
          )}
        </For>
      </div>
      <Show when={visible().length > 0}>
        <p class="mt-4 text-sm text-ink-muted">{t("kds.count", { count: visible().length })}</p>
      </Show>
    </section>
  );
}
