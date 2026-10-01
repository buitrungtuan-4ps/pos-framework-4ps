import { For, Show, createSignal, onMount } from "solid-js";

import { api } from "../api/client";
import type { ReprintResponse, SettledBill } from "../api/types";
import { PageHeader } from "../components/ui";
import { type MessageKey, t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { can } from "../state/permissions";
import { formatAmount, openBillCount, state, tableCounts, tableLabel } from "../state/store";

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

// Where a bill was paid, as staff name it: the table's label on the floor, or a counter order's
// number for the day.
function paidAt(bill: SettledBill): string {
  if (bill.table_id !== undefined) {
    return t("common.table", { label: tableLabel(bill.table_id) });
  }
  return bill.queue_number === undefined
    ? t("counter.no_queue_number")
    : t("counter.queue_number", { number: bill.queue_number });
}

// Today: the floor at a glance. A live read of the client projection, not a report — the reporting
// rollups are the cloud's (P7). Numbers, not charts, because this is a working screen on a busy
// counter.
//
// Under the figures, the bills paid today, for the guest who comes back for a copy of their receipt
// (ADR-0164): a tap on the bill, then *Reprint*. Choosing first, rather than a button on every row,
// so a tap on a crowded list prints the receipt that was meant. The copy is the original under the
// same number, marked COPY, and each one is counted — so there is no confirmation to tap through.
export function Today() {
  const counts = () => tableCounts();
  const shiftState = () =>
    state.shift === null
      ? t("today.no_shift")
      : t(SHIFT_LABELS[state.shift.state] ?? "status.shift_open");

  // Read when the screen opens. A bill paid while it is open shows at the next visit, which is soon
  // enough for a list somebody opens because a guest asked.
  const [bills, setBills] = createSignal<SettledBill[] | null>(null);
  const [chosen, setChosen] = createSignal<string | null>(null);
  const [copy, setCopy] = createSignal<ReprintResponse | null>(null);
  // The copies printed from this screen, over the count the list was read with. Kept apart from the
  // list so a press does not redraw the row the operator is looking at.
  const [copied, setCopied] = createSignal<Readonly<Record<string, number>>>({});
  const [error, setError] = createSignal<string | null>(null);

  onMount(async () => {
    try {
      setBills(await api.settledBills());
    } catch (caught) {
      setError(errorMessage(caught));
    }
  });

  const copiesOf = (bill: SettledBill) => copied()[bill.bill_id] ?? bill.copies;

  const chooseBill = (billId: string) => {
    setChosen(billId);
    setCopy(null);
    setError(null);
  };

  const reprintReceipt = async () => {
    const billId = chosen();
    if (billId === null) {
      return;
    }
    setError(null);
    try {
      const printed = await api.reprintReceipt(billId);
      setCopy(printed);
      setCopied((counts) => ({ ...counts, [billId]: printed.copy_number }));
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

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

      {/* The list is there to reprint from (`billing.receipt.reprint`, ADR-0158). */}
      <Show when={can("billing.receipt.reprint")}>
      <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("today.recent_bills")}</h2>
      <Show when={error()}>
        {(message) => (
          <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>
      <Show when={bills()}>
        {(listed) => (
          <Show
            when={listed().length > 0}
            fallback={<p class="text-sm text-ink-muted">{t("today.no_recent_bills")}</p>}
          >
            <ul class="flex flex-col gap-2">
              <For each={listed()}>
                {(bill) => (
                  <li>
                    <button
                      type="button"
                      class="flex min-h-touch w-full items-center gap-3 rounded-token border border-line bg-surface px-3 text-left"
                      classList={{ "border-2 border-accent font-semibold": chosen() === bill.bill_id }}
                      aria-pressed={chosen() === bill.bill_id}
                      data-step="chooseBill"
                      onClick={() => chooseBill(bill.bill_id)}
                    >
                      <span class="tabular-nums">
                        {t("pay.receipt", { number: bill.receipt_number })}
                      </span>
                      <span class="flex-1 text-ink-muted">{paidAt(bill)}</span>
                      <span class="tabular-nums">{formatAmount(bill.total_due)}</span>
                      <span class="w-12 text-right text-sm tabular-nums text-ink-muted">
                        {bill.settle_clock ?? ""}
                      </span>
                    </button>
                    <Show when={copiesOf(bill) > 0}>
                      <p class="mt-1 px-3 text-sm text-ink-muted" data-outcome="receipt-copies">
                        {t("today.copies", { count: copiesOf(bill) })}
                      </p>
                    </Show>
                    <Show when={chosen() === bill.bill_id}>
                      <button
                        type="button"
                        class="mt-2 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
                        data-step="reprintReceipt"
                        onClick={() => void reprintReceipt()}
                      >
                        {t("today.reprint")}
                      </button>
                      <Show when={copy()}>
                        {(printed) => (
                          <p
                            class="mt-1 text-sm text-ink-muted"
                            role="status"
                            data-outcome="receipt-reprinted"
                          >
                            {t(printOutcomeKey(printed().receipt_print, "pay.copy_printed"), {
                              number: printed().copy_number,
                            })}
                          </p>
                        )}
                      </Show>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </Show>
        )}
      </Show>
      </Show>
    </section>
  );
}
