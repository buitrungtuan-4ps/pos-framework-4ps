import { For, Show, createSignal, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import type { BillResponse, CounterOrder, PaymentRequest } from "../api/types";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { formatMoney, money, quickCashFor } from "../lib/money";
import { cashDenominations, settle, tenderAccepted, tipsEnabled } from "../state/store";

// The counter screen: the takeaway orders waiting to be paid for, and the pad that charges one
// (ADR-0093).
//
// This is the counter's floor plan. A relayed marketplace order, a public-API order or a QR counter
// order is tableless by design (ADR-0064), so it appears on no floor plan and there is no table to
// tap. Before this screen a store could accept, price, queue and fire such an order and had no way
// to take money for it — the routes existed but nothing an operator could touch reached them.
//
// The queue number is the heading of each card because it is what staff shouted at the customer and
// what the customer will say back. The order's ULID is never shown: an operator cannot read one, and
// the list is what turns the id into something tappable.
export function Takeaway() {
  const [orders, setOrders] = createSignal<CounterOrder[] | null>(null);
  const [chosen, setChosen] = createSignal<CounterOrder | null>(null);
  const [tender, setTender] = createSignal<number | null>(null);
  const [tip, setTip] = createSignal(0);
  const [done, setDone] = createSignal<BillResponse | null>(null);
  const [error, setError] = createSignal<string | null>(null);

  const explain = (caught: unknown) =>
    setError(caught instanceof ApiError ? caught.message : t("common.store_error"));

  const refresh = () =>
    api
      .openOrders()
      .then((waiting) => setOrders(waiting))
      .catch((caught: unknown) => {
        setOrders([]);
        explain(caught);
      });

  onMount(() => void refresh());

  // The amount owed, from the edge's own figure — the same `billing::assemble` the settle proves
  // against, so the number read to the customer and the number charged are one calculation.
  const total = () => chosen()?.total_due.amount_minor ?? 0;
  const currency = () => chosen()?.total_due.currency_code ?? "";
  // As on the table pay screen: the tip comes out of the change, not out of the sale, so the figure
  // shown is the one the edge records (roadmap **B1.3**).
  const change = () => {
    const offered = tender();
    const owed = total() + tip();
    return offered !== null && offered >= owed ? offered - owed : 0;
  };
  const tipKeys = () => [5, 10, 15].map((percent) => (total() * percent) / 100);
  // The same quick-cash ladder the table pay screen offers, so a cashier's hands learn one pad —
  // and, since roadmap **E5**, keyed on this order's own currency rather than VND's three notes.
  // This was the fifth hardcoded-VND site: E5 named three, the audit found a fourth, and this is the
  // one that survived the fix to `Pay.tsx` because it was written the same way one file over.
  const quickCash = () => {
    const owed = total() + tip();
    return [owed, ...quickCashFor(cashDenominations(), owed)];
  };

  const back = () => {
    setChosen(null);
    setTender(null);
    setDone(null);
    setError(null);
    void refresh();
  };

  // Open a bill on the order — or resume the one already open on it. Resuming matters: a screen that
  // reloaded mid-sale would otherwise ask for a second bill, which the edge refuses (409), and the
  // cashier would be stuck looking at an order they cannot charge.
  const charge = async (order: CounterOrder) => {
    setError(null);
    setChosen(order);
    setTender(null);
    if (order.bill_id !== undefined) {
      return;
    }
    try {
      const bill = await api.openBillForOrder(order.order_id);
      setChosen({ ...order, bill_id: bill.bill_id });
    } catch (caught) {
      explain(caught);
      setChosen(null);
    }
  };

  const pay = async (payments: PaymentRequest[]) => {
    const order = chosen();
    if (order?.bill_id === undefined) {
      return;
    }
    setError(null);
    try {
      setDone(await settle(order.bill_id, payments));
    } catch (caught) {
      explain(caught);
    }
  };

  const payCash = () => {
    const offered = tender() ?? total() + tip();
    void pay([
      {
        method: "PAYMENT_METHOD_CASH",
        tendered: money(currency(), offered),
        applied_to_bill: money(currency(), total()),
        tip: money(currency(), tip()),
      },
    ]);
  };

  const payCard = () =>
    void pay([
      {
        method: "PAYMENT_METHOD_CARD",
        tendered: money(currency(), total() + tip()),
        applied_to_bill: money(currency(), total()),
        tip: money(currency(), tip()),
      },
    ]);

  return (
    <section class="mx-auto max-w-xl p-4">
      <Show
        when={chosen()}
        fallback={
          <>
            <PageHeader title={t("counter.title")} />

            <Show when={error()}>
              {(message) => (
                <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                  {message()}
                </p>
              )}
            </Show>

            <Show
              when={(orders()?.length ?? 0) > 0}
              fallback={
                <Show when={orders() !== null}>
                  <p class="mt-6 text-ink-muted">{t("counter.empty")}</p>
                  <p class="mt-1 text-sm text-ink-muted">{t("counter.empty_hint")}</p>
                </Show>
              }
            >
              <ul class="mt-4 flex list-none flex-col gap-2 p-0">
                <For each={orders() ?? []}>
                  {(order) => (
                    <li>
                      <button
                        type="button"
                        class="min-h-touch w-full rounded-token border border-line bg-surface p-3 text-left"
                        data-step="charge"
                        onClick={() => void charge(order)}
                      >
                        <span class="flex items-baseline justify-between gap-3">
                          <span class="text-lg font-semibold tabular-nums">
                            {order.queue_number === undefined
                              ? t("counter.no_queue_number")
                              : t("counter.queue_number", { number: order.queue_number })}
                          </span>
                          <span class="text-lg font-semibold tabular-nums">
                            {formatMoney(order.total_due)}
                          </span>
                        </span>
                        <span class="mt-1 block text-sm text-ink-muted">
                          {order.items
                            .map((line) => `${line.quantity.milli / 1000} × ${line.display_name}`)
                            .join(", ")}
                        </span>
                        <Show when={order.bill_id !== undefined}>
                          <span class="mt-1 block text-sm text-accent">{t("counter.resume")}</span>
                        </Show>
                      </button>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </>
        }
      >
        {(order) => (
          <>
            <button type="button" class="border-0 bg-transparent p-0 text-sm text-ink-muted" onClick={back}>
              {t("counter.back")}
            </button>

            <Show
              when={done()}
              fallback={
                <>
                  <p class="mt-4 text-sm text-ink-muted">
                    {order().queue_number === undefined
                      ? t("counter.no_queue_number")
                      : t("counter.queue_number", { number: order().queue_number ?? 0 })}
                  </p>
                  <p class="text-2xl font-semibold tabular-nums">{formatMoney(order().total_due)}</p>

                  <Show when={error()}>
                    {(message) => (
                      <p
                        class="mt-3 rounded-token border border-danger px-3 py-2 text-danger"
                        role="alert"
                      >
                        {message()}
                      </p>
                    )}
                  </Show>

                  <Show when={tipsEnabled()}>
                    <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("pay.tip")}</h2>
                    <div class="grid grid-cols-4 gap-2">
                      <button
                        type="button"
                        class="min-h-touch rounded-token border border-line bg-surface"
                        classList={{ "border-accent": tip() === 0 }}
                        onClick={() => setTip(0)}
                      >
                        {t("pay.tip_none")}
                      </button>
                      <For each={tipKeys()}>
                        {(amount) => (
                          <button
                            type="button"
                            class="min-h-touch rounded-token border border-line bg-surface tabular-nums"
                            classList={{ "border-accent": tip() === amount && amount > 0 }}
                            data-step="setTip"
                            onClick={() => setTip(amount)}
                          >
                            {formatMoney(money(currency(), amount))}
                          </button>
                        )}
                      </For>
                    </div>
                  </Show>

                  <Show when={tenderAccepted("PAYMENT_METHOD_CASH")}>
                  <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("pay.cash")}</h2>
                  <div class="grid grid-cols-2 gap-2">
                    <For each={quickCash()}>
                      {(amount) => (
                        <button
                          type="button"
                          class="min-h-touch rounded-token border border-line bg-surface tabular-nums"
                          classList={{ "border-accent": tender() === amount }}
                          data-step="setTender"
                          onClick={() => setTender(amount)}
                        >
                          {amount === total()
                            ? t("pay.exact")
                            : formatMoney(money(currency(), amount))}
                        </button>
                      )}
                    </For>
                  </div>
                  <p class="mt-2 text-sm text-ink-muted">
                    {t("pay.change")}:{" "}
                    <span class="tabular-nums">{formatMoney(money(currency(), change()))}</span>
                  </p>
                  </Show>

                  <div class="mt-4 flex flex-col gap-2">
                    <Show when={tenderAccepted("PAYMENT_METHOD_CASH")}>
                      <button
                        type="button"
                        class="min-h-money rounded-token bg-accent text-lg font-semibold text-accent-ink"
                        data-step="payCash"
                        onClick={() => payCash()}
                      >
                        {t("pay.take_cash")}
                      </button>
                    </Show>
                    <Show when={tenderAccepted("PAYMENT_METHOD_CARD")}>
                      <button
                        type="button"
                        class="min-h-touch rounded-token border border-line bg-surface"
                        data-step="payCard"
                        onClick={() => payCard()}
                      >
                        {t("pay.card")}
                      </button>
                    </Show>
                  </div>
                </>
              }
            >
              {(settled) => (
                <>
                  <p class="mt-4 text-lg font-semibold">{t("pay.settled")}</p>
                  <Show when={settled().receipt_number}>
                    {(number) => (
                      <p class="tabular-nums">{t("pay.receipt", { number: number() })}</p>
                    )}
                  </Show>
                  <p class="mt-2 text-sm text-ink-muted">
                    {t("pay.change")}:{" "}
                    <span class="tabular-nums">{formatMoney(money(currency(), change()))}</span>
                  </p>
                  <button
                    type="button"
                    class="mt-4 min-h-touch w-full rounded-token border border-line bg-surface"
                    onClick={back}
                  >
                    {t("counter.next")}
                  </button>
                </>
              )}
            </Show>
          </>
        )}
      </Show>
    </section>
  );
}
