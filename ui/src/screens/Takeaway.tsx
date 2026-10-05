import { For, Show, createSignal, onMount } from "solid-js";
import { useNavigate, useSearchParams } from "@solidjs/router";

import { api } from "../api/client";
import type { BillResponse, CounterOrder, PaymentRequest, ReprintResponse } from "../api/types";
import { Keypad } from "../components/Keypad";
import { PageHeader } from "../components/ui";
import { t } from "../i18n";
import { money, parseWhole, percentOf, quickCashFor, roundToIncrement } from "../lib/money";
import {
  cashDenominations,
  cashRoundingIncrement,
  currencyExponent,
  formatAmount,
  settle,
  startWalkIn,
  tenderAccepted,
  tipPercents,
  tipsEnabled,
  walkInAsks,
} from "../state/store";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { can } from "../state/permissions";

// The table pay screen's guard on the cash snap, kept in step with `Pay.tsx` by hand rather than
// shared: this is its second use, and `docs/design-principles.md` extracts on the third. `Pay.tsx`
// carries the reasoning. Asked of the store's own keys, in its order, so it compares each key with
// every other rather than with the one before it.
function distinctAndSpendable(keys: readonly number[]): boolean {
  return keys.every((amount, index) => amount > 0 && keys.indexOf(amount) === index);
}

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
  // "Other amount", as on the table pay screen: the figure the guest handed over, typed on the pad.
  const [typing, setTyping] = createSignal(false);
  const [typedText, setTypedText] = createSignal("");
  const [tip, setTip] = createSignal(0);
  const [done, setDone] = createSignal<BillResponse | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  // A copy of the receipt just settled, as on the table pay screen (ADR-0164): what came of the last
  // press, or null before the first.
  const [copy, setCopy] = createSignal<ReprintResponse | null>(null);
  const navigate = useNavigate();
  // `?charge=<order>` is how the order screen hands a walk-in over to be paid (ADR-0146): the list
  // loads, and that order's pad opens as if its card had been tapped.
  const [search, setSearch] = useSearchParams<{ charge?: string }>();

  const explain = (caught: unknown) =>
    setError(errorMessage(caught));

  const refresh = () =>
    api
      .openOrders()
      .then((waiting) => {
        setOrders(waiting);
        const wanted = search.charge;
        if (wanted !== undefined) {
          setSearch({ charge: undefined });
          const order = waiting.find((candidate) => candidate.order_id === wanted);
          if (order !== undefined) {
            void charge(order);
          }
        }
      })
      .catch((caught: unknown) => {
        setOrders([]);
        explain(caught);
      });

  // A walk-in guest (ADR-0146, finding F12): the counter opens its own order and goes straight to
  // it. Before this, a store without tables could charge an order someone else started and could
  // not start one.
  const openWalkIn = async (channel?: string) => {
    setError(null);
    try {
      navigate(`/order/${await startWalkIn(channel)}`);
    } catch (caught) {
      explain(caught);
    }
  };
  // A store that asks each walk-in guest whether they eat in or take away (ADR-0160 decision 2):
  // New order offers the two answers, and the order opens on the channel tapped. Every other store
  // opens the order at the first tap, as before.
  const [asking, setAsking] = createSignal(false);
  const newOrder = () => (walkInAsks() ? setAsking(!asking()) : void openWalkIn());

  onMount(() => void refresh());

  // The amount owed, from the edge's own figure — the same `billing::assemble` the settle proves
  // against, so the number read to the customer and the number charged are one calculation.
  const total = () => chosen()?.total_due.amount_minor ?? 0;
  const currency = () => chosen()?.total_due.currency_code ?? "";
  // As on the table pay screen: the tip comes out of the change, not out of the sale, so the figure
  // shown is the one the edge records (roadmap **B1.3**).
  const typedTender = () => parseWhole(typedText(), currencyExponent());
  const offered = () => (typing() ? typedTender() : tender());
  const change = () => {
    const handed = offered();
    const owed = total() + tip();
    return handed !== null && handed >= owed ? handed - owed : 0;
  };
  const shortBy = () => {
    const typed = typing() ? typedTender() : null;
    const owed = total() + tip();
    return typed !== null && typed < owed ? owed - typed : null;
  };
  const typedTenderInvalid = () => typing() && (typedTender() === null || shortBy() !== null);
  // Opens the pad. A figure already typed survives a quick key closing the pad and this opening it
  // again, so a cashier who changes their mind twice does not type it twice.
  const typeTender = () => setTyping(true);
  // The store's own tip keys, in its order (ADR-0160 decision 2), with the table pay screen's
  // arithmetic, kept in step with `Pay.tsx`'s `tipKeys` by hand: whole minor units, snapped to the
  // store's cash increment where that still leaves amounts a cashier can tell apart. This line was
  // `(total * percent) / 100`, the float division `Pay.tsx` had already been fixed for: on an odd
  // total the 5% key was a fraction of a đồng, and the edge refused the settle with the guest's money
  // on the counter. It then offered 5, 10 and 15 percent at every store after the table pay screen
  // took the store's own keys.
  const tipKeys = () => {
    const exact = tipPercents().map((percent) => percentOf(total(), percent));
    const increment = cashRoundingIncrement();
    if (increment === null) {
      return exact;
    }
    const snapped = exact.map((amount) => roundToIncrement(amount, increment));
    return distinctAndSpendable(snapped) ? snapped : exact;
  };
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
    setTyping(false);
    setTypedText("");
    setDone(null);
    setCopy(null);
    setError(null);
    void refresh();
  };

  const printAgain = async (billId: string) => {
    setError(null);
    try {
      setCopy(await api.reprintReceipt(billId));
    } catch (caught) {
      explain(caught);
    }
  };

  // Open a bill on the order — or resume the one already open on it. Resuming matters: a screen that
  // reloaded mid-sale would otherwise ask for a second bill, which the edge refuses (409), and the
  // cashier would be stuck looking at an order they cannot charge.
  const charge = async (order: CounterOrder) => {
    setError(null);
    setChosen(order);
    setTender(null);
    setTyping(false);
    setTypedText("");
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
    const handed = offered() ?? total() + tip();
    void pay([
      {
        method: "PAYMENT_METHOD_CASH",
        tendered: money(currency(), handed),
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

  // A transfer to the store's QR, recorded once the cashier has seen it arrive — see `Pay.tsx`.
  const payQr = () =>
    void pay([
      {
        method: "PAYMENT_METHOD_QR",
        tendered: money(currency(), total() + tip()),
        applied_to_bill: money(currency(), total()),
        tip: money(currency(), tip()),
      },
    ]);

  return (
    // The mark that proves this screen rendered, which is what a counter store's home must show
    // (`docs/ui-ux.md` §3). Named for the role rather than the screen: `/counter` is this list on
    // any store, and on a store with no tables it is also `/`.
    <section class="mx-auto max-w-xl p-4" data-outcome="counter">
      <Show
        when={chosen()}
        fallback={
          <>
            <PageHeader title={t("counter.title")} />
            <Show when={can("sales.line.add")}>
            <button
              type="button"
              class="mt-3 min-h-touch w-full rounded-token bg-primary px-4 text-primary-ink"
              aria-expanded={walkInAsks() ? asking() : undefined}
              data-step="newOrder"
              onClick={() => newOrder()}
            >
              {t("counter.new_order")}
            </button>
            <Show when={walkInAsks() && asking()}>
              <div class="mt-2 grid grid-cols-2 gap-2">
                <button
                  type="button"
                  class="min-h-money rounded-token border border-primary px-4 text-lg font-semibold text-ink"
                  data-step="eatIn"
                  onClick={() => void openWalkIn("SALES_CHANNEL_DINE_IN")}
                >
                  {t("counter.eat_in")}
                </button>
                <button
                  type="button"
                  class="min-h-money rounded-token border border-primary px-4 text-lg font-semibold text-ink"
                  data-step="takeAway"
                  onClick={() => void openWalkIn("SALES_CHANNEL_TAKEAWAY")}
                >
                  {t("counter.take_away")}
                </button>
              </div>
            </Show>
            </Show>

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
                        class="min-h-touch w-full rounded-token border border-line bg-surface p-3 text-left disabled:opacity-50"
                        disabled={order.bill_id === undefined && !can("billing.bill.open")}
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
                            {formatAmount(order.total_due)}
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
            <button
              type="button"
              class="min-h-touch border-0 bg-transparent p-0 text-sm text-ink-muted hover:underline focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-token"
              aria-label={t("counter.back")}
              onClick={back}
            >
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
                  <p class="text-2xl font-semibold tabular-nums">{formatAmount(order().total_due)}</p>

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

                  {/* The tip and the tenders are taking payment (ADR-0158). */}
                  <Show when={can("billing.payment.take")}>
                  {/* No tip key left to offer, and the row is not drawn, as on the table pay screen:
                      with no amount to type, a lone "No tip" would offer nothing. */}
                  <Show when={tipsEnabled() && tipPercents().length > 0}>
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
                            {formatAmount(money(currency(), amount))}
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
                          classList={{ "border-accent": !typing() && tender() === amount }}
                          data-step="setTender"
                          onClick={() => {
                            setTyping(false);
                            setTender(amount);
                          }}
                        >
                          {amount === total()
                            ? t("pay.exact")
                            : formatAmount(money(currency(), amount))}
                        </button>
                      )}
                    </For>
                    <button
                      type="button"
                      class="min-h-touch rounded-token border border-line bg-surface"
                      classList={{ "border-accent": typing() }}
                      aria-expanded={typing()}
                      data-step="typeTender"
                      onClick={() => typeTender()}
                    >
                      {t("pay.other_amount")}
                    </button>
                  </div>
                  <Show when={typing()}>
                    <p class="mt-3 text-sm text-ink-muted">{t("pay.tendered")}</p>
                    <p class="text-2xl font-semibold tabular-nums" data-outcome="typed-tender">
                      {typedTender() === null
                        ? "—"
                        : formatAmount(money(currency(), typedTender() ?? 0))}
                    </p>
                    <Keypad value={typedText()} onChange={setTypedText} data-step="tenderKeypad" />
                    <Show when={shortBy()}>
                      {(gap) => (
                        <p class="mt-2 text-sm text-danger" role="status">
                          {t("pay.short_by", { amount: formatAmount(money(currency(), gap())) })}
                        </p>
                      )}
                    </Show>
                  </Show>
                  <p class="mt-2 text-sm text-ink-muted">
                    {t("pay.change")}:{" "}
                    <span class="tabular-nums">{formatAmount(money(currency(), change()))}</span>
                  </p>
                  </Show>

                  <div class="mt-4 flex flex-col gap-2">
                    <Show when={tenderAccepted("PAYMENT_METHOD_CASH")}>
                      <button
                        type="button"
                        class="min-h-money rounded-token bg-primary text-lg font-semibold text-primary-ink disabled:opacity-50"
                        disabled={typedTenderInvalid()}
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
                    <Show when={tenderAccepted("PAYMENT_METHOD_QR")}>
                      <button
                        type="button"
                        class="min-h-touch rounded-token border border-line bg-surface"
                        data-step="payQr"
                        onClick={() => payQr()}
                      >
                        {t("pay.qr")}
                      </button>
                      <p class="text-sm text-ink-muted">{t("pay.qr_hint")}</p>
                    </Show>
                  </div>
                  </Show>
                </>
              }
            >
              {(settled) => (
                <>
                  <p class="mt-4 text-lg font-semibold" data-outcome="settled">
                    {t("pay.settled")}
                  </p>
                  <Show when={settled().receipt_number}>
                    {(number) => (
                      <p class="tabular-nums">{t("pay.receipt", { number: number() })}</p>
                    )}
                  </Show>
                  <p class="mt-2 text-sm text-ink-muted">
                    {t("pay.change")}:{" "}
                    <span class="tabular-nums">{formatAmount(money(currency(), change()))}</span>
                  </p>
                  <Show when={settled().drawer_open === "DRAWER_UNAVAILABLE"}>
                    <p class="mt-1 text-sm text-danger" role="status" data-outcome="drawer-unavailable">
                      {t("pay.drawer_unavailable")}
                    </p>
                  </Show>
                  <Show when={can("billing.receipt.reprint")}>
                  <button
                    type="button"
                    class="mt-3 min-h-touch w-full rounded-token border border-line text-sm"
                    data-step="printAgain"
                    onClick={() => void printAgain(settled().bill_id)}
                  >
                    {t("pay.print_again")}
                  </button>
                  </Show>
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
