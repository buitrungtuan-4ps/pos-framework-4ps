import { For, Show, createSignal, onMount } from "solid-js";

import { ApiError, api } from "../api/client";
import type { BillResponse, CounterOrder, PaymentRequest } from "../api/types";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { formatMoney, money } from "../lib/money";
import { settle } from "../state/store";

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
  const change = () => {
    const offered = tender();
    return offered !== null && offered >= total() ? offered - total() : 0;
  };
  // The same VND quick-cash ladder the table pay screen offers, so a cashier's hands learn one pad.
  const quickCash = () => [total(), 50_000, 100_000, 200_000].filter((amount) => amount >= total());

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
    const offered = tender() ?? total();
    void pay([
      {
        method: "PAYMENT_METHOD_CASH",
        tendered: money(currency(), offered),
        applied_to_bill: money(currency(), total()),
      },
    ]);
  };

  const payCard = () =>
    void pay([
      {
        method: "PAYMENT_METHOD_CARD",
        tendered: money(currency(), total()),
        applied_to_bill: money(currency(), total()),
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

                  <h2 class="mt-6 mb-2 text-sm font-semibold text-ink-muted">{t("pay.cash")}</h2>
                  <div class="grid grid-cols-2 gap-2">
                    <For each={quickCash()}>
                      {(amount) => (
                        <button
                          type="button"
                          class="min-h-touch rounded-token border border-line bg-surface tabular-nums"
                          classList={{ "border-accent": tender() === amount }}
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

                  <div class="mt-4 flex flex-col gap-2">
                    <button
                      type="button"
                      class="min-h-money rounded-token bg-accent text-lg font-semibold text-accent-ink"
                      onClick={() => payCash()}
                    >
                      {t("pay.take_cash")}
                    </button>
                    <button
                      type="button"
                      class="min-h-touch rounded-token border border-line bg-surface"
                      onClick={() => payCard()}
                    >
                      {t("pay.card")}
                    </button>
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
