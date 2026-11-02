import { For, Show, createEffect, createSignal, on, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import type {
  ReprintResponse,
  SettledBill,
  TakingsResponse,
  VoidSettledResponse,
} from "../api/schemas";
import { ApproverFields } from "../components/ApproverFields";
import { ReadFailed } from "../components/StoreData";
import { PageHeader } from "../components/ui";
import { type MessageKey, t } from "../i18n";
import { tableStateKey } from "../i18n/labels";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { asksApprover, can, holdsOwn } from "../state/permissions";
import { retryable } from "../state/engine";
import { createScreenRead } from "../state/screen-read";
import {
  formatAmount,
  openBillCount,
  reasonsFor,
  state,
  tableCounts,
  tableLabel,
} from "../state/store";

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
// Beside them, what the store has taken today (ADR-0160 decision 2): the day's settled bills at what
// their guests paid, and how many, for a person whose own role grants `reports.takings.view`. The
// store's figure and nothing finer — no person, no till — because takings per person would be
// monitoring staff.
//
// Under the figures, the bills paid today, for the guest who comes back for a copy of their receipt
// (ADR-0164): a tap on the bill, then *Reprint*. Choosing first, rather than a button on every row,
// so a tap on a crowded list prints the receipt that was meant. The copy is the original under the
// same number, marked COPY, and each one is counted — so there is no confirmation to tap through.
// The takings tile's read. A refusal, or an edge too old to answer, hides the tile rather than
// putting an error on a working screen; a blip is retried, quietly.
function createTakingsRead(setTakings: (takings: TakingsResponse | null) => void) {
  return createScreenRead(() => api.takings(), setTakings, {
    handled: (caught) => {
      setTakings(null);
      return caught instanceof ApiError && !retryable(caught);
    },
  });
}

// The floor's counts, the open bills, the shift and, for whoever may see them, today's takings.
function Tiles(props: { takings: TakingsResponse | null }) {
  const counts = () => tableCounts();
  const shiftState = () =>
    state.shift === null
      ? t("today.no_shift")
      : t(SHIFT_LABELS[state.shift.state] ?? "status.shift_open");
  return (
    <>
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
        <Show when={props.takings}>
          {(taken) => (
            <div class="rounded-token border border-line bg-surface p-4" data-outcome="today-takings">
              <p class="text-2xl font-semibold tabular-nums">
                {formatAmount(taken().takings_amount)}
              </p>
              <p class="text-sm text-ink-muted">{t("today.takings")}</p>
              <p class="text-sm text-ink-muted tabular-nums">
                {t("today.takings_bills", { count: taken().bill_count })}
              </p>
            </div>
          )}
        </Show>
      </div>

    </>
  );
}

// A paid bill voided whole (ADR-0184): *Void bill* under the chosen bill, then a reason from the
// store's list, with the approver's code and PIN typed above it where the person needs one — the
// bill void's own shape. The edge refuses a bill past the store's window or with an issued
// invoice, and the panel says so in place. What it gives back is the edge's figure.
function VoidPaidBill(props: {
  bill: SettledBill;
  onVoided: (voided: VoidSettledResponse) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const [code, setCode] = createSignal("");
  const [pin, setPin] = createSignal("");
  const [error, setError] = createSignal<string | null>(null);
  const asks = () => asksApprover("billing.bill.void_settled");
  const typed = () => code().trim() !== "" && pin() !== "";
  const reasons = () => reasonsFor("REASON_ACTION_VOID_BILL");

  const askVoidPaidBill = () => {
    setError(null);
    setOpen(true);
  };
  const voidPaidBillReason = async (reasonCodeId: string) => {
    setError(null);
    const approval =
      asks() && typed() ? { approver_code: code().trim(), approver_pin: pin() } : {};
    try {
      const voided = await api.voidSettledBill(props.bill.bill_id, {
        reason_code_id: reasonCodeId,
        ...approval,
      });
      setOpen(false);
      props.onVoided(voided);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  return (
    <Show
      when={open()}
      fallback={
        <button
          type="button"
          class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50"
          disabled={reasons().length === 0}
          data-step="askVoidPaidBill"
          onClick={() => askVoidPaidBill()}
        >
          {t("today.void")}
        </button>
      }
    >
      <div class="mt-2 rounded-token border border-line bg-surface p-3">
        <h3 class="font-semibold">{t("today.void_title")}</h3>
        <p class="mt-1 text-sm text-ink-muted">{t("today.void_explain")}</p>
        <Show when={asks()}>
          <p class="mt-1 text-sm text-ink-muted">{t("today.void_manager")}</p>
          <ApproverFields
            id="void-paid-bill-approver"
            code={code()}
            pin={pin()}
            onCode={setCode}
            onPin={setPin}
            codeLabel={t("pay.approver_code")}
            pinLabel={t("pay.approver_pin")}
          />
        </Show>
        <p class="mt-3 text-sm text-ink-muted">{t("pay.void_reason")}</p>
        <div class="mt-2 grid grid-cols-2 gap-2">
          <For each={reasons()}>
            {(reason) => (
              <button
                type="button"
                class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                disabled={asks() && !typed()}
                data-step="voidPaidBillReason"
                onClick={() => void voidPaidBillReason(reason.reason_code_id)}
              >
                {reason.display_name}
              </button>
            )}
          </For>
        </div>
        <Show when={error()}>
          {(message) => (
            <p class="mt-2 text-sm text-danger" role="alert">
              {message()}
            </p>
          )}
        </Show>
        <button
          type="button"
          class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
          onClick={() => setOpen(false)}
        >
          {t("common.cancel")}
        </button>
      </div>
    </Show>
  );
}

// What a void after payment gave back, as the edge answered it: the cash to hand back, the card to
// reverse on the terminal, and what came of the slip.
function VoidedOutcome(props: { voided: VoidSettledResponse }) {
  return (
    <div class="mt-2 text-sm" role="status" data-outcome="paid-bill-voided">
      <p class="font-semibold">
        {t("today.voided_done", { number: props.voided.receipt_number })}
      </p>
      <Show when={props.voided.cash_returned.amount_minor > 0}>
        <p>{t("today.hand_back_cash", { amount: formatAmount(props.voided.cash_returned) })}</p>
      </Show>
      <Show when={props.voided.other_reversed.amount_minor > 0}>
        <p>
          {t("today.reverse_on_terminal", {
            amount: formatAmount(props.voided.other_reversed),
          })}
        </p>
      </Show>
      <p class="text-ink-muted">
        {t(printOutcomeKey(props.voided.void_slip_print, "today.void_slip_printed"))}
      </p>
    </div>
  );
}

export function Today() {
  // Read when the screen opens. A bill paid while it is open shows at the next visit, which is soon
  // enough for a list somebody opens because a guest asked.
  const [bills, setBills] = createSignal<SettledBill[] | null>(null);
  const [chosen, setChosen] = createSignal<string | null>(null);
  const [copy, setCopy] = createSignal<ReprintResponse | null>(null);
  // The copies printed from this screen, over the count the list was read with. Kept apart from the
  // list so a press does not redraw the row the operator is looking at.
  const [copied, setCopied] = createSignal<Readonly<Record<string, number>>>({});
  const [error, setError] = createSignal<string | null>(null);
  const [takings, setTakings] = createSignal<TakingsResponse | null>(null);
  // The bills voided from this screen, over what the list was read with (ADR-0184).
  const [voidedHere, setVoidedHere] = createSignal<Readonly<Record<string, VoidSettledResponse>>>(
    {},
  );
  const isVoided = (bill: SettledBill) => bill.voided || voidedHere()[bill.bill_id] !== undefined;

  // Read when the person may see it, and again if that changes while the screen is open. A refusal,
  // or an edge too old to answer, hides the tile rather than putting an error on a working screen.
  const takingsRead = createTakingsRead(setTakings);
  createEffect(
    on(
      () => holdsOwn("reports.takings.view"),
      (sees) => {
        if (!sees) {
          setTakings(null);
          return;
        }
        void takingsRead.run();
      },
    ),
  );

  // Retried with the engine's backoff while the screen is open, and shown in place when it fails.
  const billsRead = createScreenRead(() => api.settledBills(), setBills);
  onMount(() => void billsRead.run());

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
      <Tiles takings={takings()} />

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
      <ReadFailed read={billsRead} />
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
                      <Show when={isVoided(bill)}>
                        <span class="text-sm font-semibold text-danger">{t("today.voided")}</span>
                      </Show>
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
                      <Show when={voidedHere()[bill.bill_id]}>
                        {(voided) => <VoidedOutcome voided={voided()} />}
                      </Show>
                      <Show when={!isVoided(bill) && can("billing.bill.void_settled")}>
                        <VoidPaidBill
                          bill={bill}
                          onVoided={(voided) =>
                            setVoidedHere((done) => ({ ...done, [bill.bill_id]: voided }))
                          }
                        />
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
