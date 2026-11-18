// What a bill taking its payments one at a time has been paid (ADR-0220 decision 3): the edge's
// list of payments (how, how much and when, nobody named), what is paid and what remains, and in an
// even split the share the next guest owes. Read from the bill's own check, so a share taken on
// another till, or before a reload, is here too. A payment taken is the edge's record, so there is
// no undo: a guest who should not have paid is a void, which gives each payment back.

import { For, Show } from "solid-js";

import type { CheckPayment, CheckResponse } from "../api/schemas";
import { locale, t, type MessageKey } from "../i18n";
import { money } from "../lib/money";
import { formatAmount } from "../state/store";

// The label for a payment method in the list.
function methodKey(method: string): MessageKey {
  switch (method) {
    case "PAYMENT_METHOD_CARD":
      return "pay.method_card";
    case "PAYMENT_METHOD_QR":
      return "pay.method_qr";
    default:
      return "pay.method_cash";
  }
}

// When a payment was taken, as a clock time in the reader's own language.
function takenAt(time: string): string {
  return new Date(Date.parse(time)).toLocaleTimeString(locale() === "vi" ? "vi-VN" : "en-GB", {
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function PartPayments(props: {
  /** The bill's read; nothing is shown until it has a payment or a split is chosen. */
  readonly check: CheckResponse | null;
  /** How many equal shares, or null outside an even split. */
  readonly ways: number | null;
  /** What the next guest owes in a split. */
  readonly due: number;
  /** The change owed on the share just taken, shown until the next. */
  readonly change: number;
  readonly onCancel: () => void;
}) {
  const payments = (): readonly CheckPayment[] => props.check?.payments ?? [];
  const currency = () => props.check?.total_due.currency_code ?? "";
  return (
    <Show when={props.ways !== null || payments().length > 0}>
      <div class="mt-3 rounded-token border border-line bg-surface p-3" data-outcome="part-payments">
        <Show when={props.ways}>
          {(n) => (
            <>
              <p class="text-sm text-ink-muted">{t("pay.share_of", { part: payments().length + 1, count: n() })}</p>
              <p class="text-2xl font-semibold tabular-nums" data-outcome="share-due">
                {formatAmount(money(currency(), props.due))}
              </p>
            </>
          )}
        </Show>
        <Show when={props.change > 0}>
          <p class="mt-1 text-sm font-semibold" role="status" data-outcome="share-change">
            {t("pay.change")}: <span class="tabular-nums">{formatAmount(money(currency(), props.change))}</span>
          </p>
        </Show>
        <ul class="mt-2 flex list-none flex-col gap-1 p-0">
          <For each={payments()}>
            {(payment, index) => (
              <li class="flex items-center justify-between gap-2 text-sm" data-outcome="share-taken">
                <span>
                  {t("pay.share_taken", { part: index() + 1, method: t(methodKey(payment.method)) })} ·{" "}
                  {takenAt(payment.taken_time)}
                </span>
                <span class="tabular-nums">{formatAmount(payment.amount)}</span>
              </li>
            )}
          </For>
        </ul>
        <Show when={props.check?.paid_total}>
          {(paid) => (
            <p class="mt-2 text-sm text-ink-muted" data-outcome="paid-so-far">
              {t("pay.paid_so_far")}: <span class="tabular-nums">{formatAmount(paid())}</span> · {t("pay.remaining")}:{" "}
              <span class="tabular-nums">{formatAmount(props.check?.remaining ?? paid())}</span>
            </p>
          )}
        </Show>
        <Show when={props.ways !== null && payments().length === 0}>
          <button
            type="button"
            class="mt-2 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted"
            onClick={() => props.onCancel()}
          >
            {t("pay.split_cancel")}
          </button>
        </Show>
      </div>
    </Show>
  );
}
