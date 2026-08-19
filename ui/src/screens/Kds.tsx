import { For, Show, createSignal } from "solid-js";

import { t } from "../i18n";
import { useDarkTakeover } from "../lib/screen";
import { firedLines } from "../state/store";

// The kitchen display: every fired line, large, on a dark panel. Bumping a ticket clears it from
// this screen. That acknowledgement is local for now — the domain has no "line made" transition yet,
// so a bump does not travel; a durable bump event is the follow-up that lets a second KDS agree.
export function Kds() {
  useDarkTakeover();
  const [bumped, setBumped] = createSignal<ReadonlySet<string>>(new Set());
  const visible = () => firedLines().filter((line) => !bumped().has(line.orderLineId));
  const bump = (orderLineId: string) =>
    setBumped((current) => new Set(current).add(orderLineId));

  return (
    <section class="p-4">
      <h1 class="mb-4 text-xl font-semibold">{t("kds.title")}</h1>
      <div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4">
        <For each={visible()} fallback={<p class="text-ink-muted">{t("kds.empty")}</p>}>
          {(line) => (
            <button
              type="button"
              class="flex min-h-money flex-col items-start gap-1 rounded-token border border-line bg-surface-raised p-4 text-left"
              onClick={() => bump(line.orderLineId)}
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
