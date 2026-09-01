import { For, Show, createSignal, onMount } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import type { BillResponse, CheckResponse, PaymentRequest } from "../api/types";
import { t } from "../i18n";
import { formatMoney, money } from "../lib/money";
import { loadCheck, openBill, openBillFor, settle } from "../state/store";

// The pay screen: the amount owed large, a cash pad with the VND quick-cash denominations and its
// change, or card for the exact amount. On settlement it shows the gapless receipt number and the
// change to hand back; the table is now the floor's to clean.
export function Pay() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [billId, setBillId] = createSignal<string | null>(openBillFor(params.id) ?? null);
  const [error, setError] = createSignal<string | null>(null);
  const [tender, setTender] = createSignal<number | null>(null);
  const [done, setDone] = createSignal<BillResponse | null>(null);
  // What the guest owes, as the edge assembled it (E5). Null until it arrives; the screen shows no
  // amount rather than a guess, because the till no longer has the means to guess one.
  const [check, setCheck] = createSignal<CheckResponse | null>(null);

  onMount(() => {
    void loadCheck(params.id)
      .then((totals) => setCheck(totals))
      .catch((caught: unknown) =>
        setError(caught instanceof ApiError ? caught.message : t("common.store_error")),
      );
    if (billId() === null) {
      openBill(params.id)
        .then((id) => setBillId(id))
        .catch((caught: unknown) =>
          setError(caught instanceof ApiError ? caught.message : t("common.store_error")),
        );
    }
  });

  // The edge's figure, in minor units, for the tender pad's arithmetic. Zero until the check lands,
  // which disables nothing the operator can get wrong: paying is refused without an amount owed.
  const total = () => check()?.total_due.amount_minor ?? 0;
  // The store's currency, taken from the edge's own figure rather than assumed — a store on any other
  // currency then renders and tenders correctly with no change here.
  const currency = () => check()?.total_due.currency_code ?? "";
  // Change is a subtraction of two amounts the operator can see — the edge's total and the note they
  // chose — shown as they tap. The authoritative figure is the one the settle records per payment
  // (`change_given`), which the receipt carries.
  const change = () => {
    const chosen = tender();
    return chosen !== null && chosen >= total() ? chosen - total() : 0;
  };

  // The archive's VND quick-cash: 50k / 100k / 200k, plus the exact amount.
  const quickCash = () => [total(), 50_000, 100_000, 200_000].filter((amount) => amount >= total());

  const pay = async (payments: PaymentRequest[]) => {
    const id = billId();
    if (id === null) {
      return;
    }
    setError(null);
    try {
      setDone(await settle(id, payments));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  const payCash = () => {
    const chosen = tender() ?? total();
    void pay([
      {
        method: "PAYMENT_METHOD_CASH",
        tendered: money(currency(), chosen),
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
      <a href={`/table/${params.id}`} class="text-sm text-ink-muted no-underline">
        {t("common.back_order")}
      </a>

      <Show
        when={done()}
        fallback={
          <>
            <p class="mt-4 text-sm text-ink-muted">{t("pay.amount_due")}</p>
            <Show when={check()} fallback={<p class="text-2xl font-semibold tabular-nums">{"—"}</p>}>
              {(totals) => (
                <p class="text-2xl font-semibold tabular-nums">{formatMoney(totals().total_due)}</p>
              )}
            </Show>

            <Show when={error()}>
              {(message) => (
                <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
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
                    {amount === total() ? t("pay.exact") : formatMoney(money(currency(), amount))}
                  </button>
                )}
              </For>
            </div>
            <p class="mt-2 text-sm text-ink-muted">
              {t("pay.change")}: <span class="tabular-nums">{formatMoney(money(currency(), change()))}</span>
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
        {(bill) => (
          <div class="mt-6 rounded-token border border-line bg-surface p-4">
            <p class="text-lg font-semibold text-ok">{t("pay.settled")}</p>
            <p class="mt-2 tabular-nums">
              {t("pay.receipt", { number: bill().receipt_number ?? 0 })}
            </p>
            <p class="mt-1 text-ink-muted">
              {t("pay.change")}: <span class="tabular-nums">{formatMoney(money(currency(), change()))}</span>
            </p>
            <Show when={bill().print_receipt}>
              <p class="mt-1 text-sm text-ink-muted">{t("pay.printing")}</p>
            </Show>
            <button
              type="button"
              class="mt-4 min-h-touch w-full rounded-token bg-accent font-semibold text-accent-ink"
              onClick={() => navigate("/")}
            >
              {t("pay.back_floor")}
            </button>
          </div>
        )}
      </Show>
    </section>
  );
}
