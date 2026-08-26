import { For } from "solid-js";

import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { openBillCount, state, tableCounts } from "../state/store";

const ORDER = [
  "TABLE_STATE_FREE",
  "TABLE_STATE_OCCUPIED",
  "TABLE_STATE_AWAITING_PAYMENT",
  "TABLE_STATE_NEEDS_CLEANING",
];

// Today: the floor at a glance. A live read of the client projection, not a report — the reporting
// rollups are the cloud's (P7). Numbers, not charts, because this is a working screen on a busy
// counter.
export function Today() {
  const counts = () => tableCounts();
  const shiftState = () =>
    state.shift === null ? t("today.no_shift") : state.shift.state.replace("SHIFT_STATE_", "").toLowerCase();

  return (
    <section class="p-4">
      <PageHeader title={t("today.title")} />
      <div class="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <For each={ORDER}>
          {(key) => (
            <div class="rounded-token border border-line bg-surface p-4">
              <p class="text-2xl font-semibold tabular-nums">{counts()[key] ?? 0}</p>
              <p class="text-sm text-ink-muted">{t(tableStateKey(key))}</p>
            </div>
          )}
        </For>
      </div>

      <div class="mt-4 grid grid-cols-2 gap-3 sm:grid-cols-4">
        <div class="rounded-token border border-line bg-surface p-4">
          <p class="text-2xl font-semibold tabular-nums">{openBillCount()}</p>
          <p class="text-sm text-ink-muted">{t("today.open_bills")}</p>
        </div>
        <div class="rounded-token border border-line bg-surface p-4">
          <p class="text-2xl font-semibold capitalize">{shiftState()}</p>
          <p class="text-sm text-ink-muted">{t("today.shift")}</p>
        </div>
      </div>
    </section>
  );
}
