import { For } from "solid-js";

import { PageHeader } from "../components/ui";
import { type MessageKey, t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { openBillCount, state, tableCounts } from "../state/store";

const ORDER = [
  "TABLE_STATE_FREE",
  "TABLE_STATE_OCCUPIED",
  "TABLE_STATE_AWAITING_PAYMENT",
  "TABLE_STATE_NEEDS_CLEANING",
];

// The shift's state as a sentence, the status bar's own labels. The tile used to lower-case the
// wire token and let CSS capitalise it, so a Vietnamese till read "None Open" or "Open" in English.
const SHIFT_LABELS: Readonly<Record<string, MessageKey>> = {
  SHIFT_STATE_OPEN: "status.shift_open",
  SHIFT_STATE_COUNTED: "status.shift_counted",
  SHIFT_STATE_CLOSED: "status.shift_closed",
};

// Today: the floor at a glance. A live read of the client projection, not a report — the reporting
// rollups are the cloud's (P7). Numbers, not charts, because this is a working screen on a busy
// counter.
export function Today() {
  const counts = () => tableCounts();
  const shiftState = () =>
    state.shift === null
      ? t("today.no_shift")
      : t(SHIFT_LABELS[state.shift.state] ?? "status.shift_open");

  return (
    <section class="p-4">
      <PageHeader title={t("today.title")} />
      <div class="grid grid-cols-2 gap-3 tablet:grid-cols-4">
        <For each={ORDER}>
          {(key) => (
            <div class="rounded-token border border-line bg-surface p-4">
              <p class="text-2xl font-semibold tabular-nums">{counts()[key] ?? 0}</p>
              <p class="text-sm text-ink-muted">{t(tableStateKey(key))}</p>
            </div>
          )}
        </For>
      </div>

      <div class="mt-4 grid grid-cols-2 gap-3 tablet:grid-cols-4">
        <div class="rounded-token border border-line bg-surface p-4">
          <p class="text-2xl font-semibold tabular-nums">{openBillCount()}</p>
          <p class="text-sm text-ink-muted">{t("today.open_bills")}</p>
        </div>
        <div class="rounded-token border border-line bg-surface p-4">
          {/* A sentence, not a figure, so it is set as text rather than at the size of the counts. */}
          <p class="text-lg font-semibold" data-outcome="today-shift">
            {shiftState()}
          </p>
          <p class="text-sm text-ink-muted">{t("today.shift")}</p>
        </div>
      </div>
    </section>
  );
}
