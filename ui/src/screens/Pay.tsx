import { For, Show, createSignal, onMount } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import type { BillResponse, BuyerRequest, CheckResponse, PaymentRequest } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { formatMoney, money, quickCashFor } from "../lib/money";
import {
  cashDenominations,
  loadCheck,
  openBill,
  openBillFor,
  settle,
  tenderAccepted,
  tipsEnabled,
} from "../state/store";

// The pay screen: the amount owed large, a cash pad with this currency's quick-cash denominations
// and its change, an optional tip, or card for the exact amount. On settlement it shows the gapless
// receipt number and the change to hand back; the table is now the floor's to clean.
//
// # The tip is optional, and that is a budget decision as much as a design one
//
// `docs/ui-ux.md` §6 caps a cash settle at three taps and this flow already spends all three
// (pay -> note -> take). A tip pad behind a button would be a fourth *required* tap and the step
// budget would fail the build, correctly. So the tip row sits in the flow already visible: a
// cashier who takes no tip taps nothing extra, and the tip's own taps are declared as their own
// task in `ui/scripts/step-budget.mjs`.
/**
 * What the till says about the receipt (ADR-0100). An edge built before C2 sends no
 * `receipt_print`, so the old "Printing receipt…" wording is the fallback — the one case where the
 * till genuinely does not know whether paper came out.
 */
function receiptPrintKey(outcome: string | undefined): MessageKey {
  switch (outcome) {
    case "PRINTED":
      return "pay.printed";
    case "NO_PRINTER":
      return "pay.print_no_printer";
    case "PRINTER_UNAVAILABLE":
      return "pay.print_unavailable";
    case "UNPRINTABLE_TEXT":
      return "pay.print_unprintable";
    default:
      return "pay.printing";
  }
}

export function Pay() {
  const params = useParams<{ id: string }>();
  const navigate = useNavigate();
  const [billId, setBillId] = createSignal<string | null>(openBillFor(params.id) ?? null);
  const [error, setError] = createSignal<string | null>(null);
  const [tender, setTender] = createSignal<number | null>(null);
  // The tip, in minor units. Zero rather than null: there is no difference between "no tip" and "a
  // tip of nothing", and `Payment.tip` is optional on the wire so zero is what the edge would have
  // defaulted to anyway.
  const [tip, setTip] = createSignal(0);
  const [done, setDone] = createSignal<BillResponse | null>(null);
  // The corporate buyer, when a company asks for a tax invoice (ADR-0107). Behind a disclosure the
  // cashier only opens on request, so an ordinary sale costs no extra tap and the step budget for
  // the cash flow is untouched. Held here for the length of one settle and never stored: the till
  // keeps no customer list, which is the point — a cross-bill index of buyers is a profiling
  // feature with its own consent posture, not a side effect of issuing invoices.
  const [buyerName, setBuyerName] = createSignal("");
  const [buyerTaxCode, setBuyerTaxCode] = createSignal("");
  const [buyerAddress, setBuyerAddress] = createSignal("");
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
  // The tip comes out of the change, not out of the bill: the guest hands over one amount, the sale
  // takes its total and the tip is what is left behind on purpose. Subtracting it here is what makes
  // the figure on screen the same one the edge records — B1.3's second defect was exactly this
  // subtraction missing on the edge, so a till told a cashier to hand back money the guest had left.
  const change = () => {
    const chosen = tender();
    const owed = total() + tip();
    return chosen !== null && chosen >= owed ? chosen - owed : 0;
  };

  // Tip keys as a share of the bill, plus a clear. Percentages rather than fixed amounts so they
  // scale with the check, and computed in minor units with integer arithmetic — the workspace bans
  // floats in a money path, and rounding a tip by accident is the kind of cent nobody can explain.
  const tipKeys = () => [5, 10, 15].map((percent) => (total() * percent) / 100);

  // The exact amount, plus this bill's own currency's banknotes that would cover it (roadmap E5).
  // These were VND's three notes regardless of where the store was, so a store on any other
  // currency was offered keys for amounts its guests cannot hand over. Keyed on `currency()` — the
  // bill's, not the store's — for the same reason every other figure here is: it is the edge's
  // answer for *this* bill. Before the check loads that is `""`, which has no note table and so
  // offers the exact amount alone; the total is zero then anyway.
  // Keyed on the sale *plus* the tip: with a tip added, a note that only covers the sale is not
  // enough money, and offering it would hand the cashier a key that cannot settle.
  const quickCash = () => {
    const owed = total() + tip();
    return [owed, ...quickCashFor(cashDenominations(), owed)];
  };

  // What the cashier typed, or nothing. A blank name means no buyer: the one field both Japan's
  // qualified invoice and India's Rule 46 require is the name, so a tax code with no name beside it
  // is an incomplete document rather than a partial one.
  const buyer = (): BuyerRequest | undefined => {
    const name = buyerName().trim();
    if (name === "") {
      return undefined;
    }
    const optional = (value: string) => (value.trim() === "" ? undefined : value.trim());
    return {
      name,
      tax_code: optional(buyerTaxCode()),
      address: optional(buyerAddress()),
    };
  };

  const pay = async (payments: PaymentRequest[]) => {
    const id = billId();
    if (id === null) {
      return;
    }
    setError(null);
    try {
      setDone(await settle(id, payments, buyer()));
    } catch (caught) {
      setError(caught instanceof ApiError ? caught.message : t("common.store_error"));
    }
  };

  const payCash = () => {
    // No note chosen means "exact", and exact now means the sale plus the tip — otherwise adding a
    // tip and tapping straight through would tender less than the guest owes.
    const chosen = tender() ?? total() + tip();
    void pay([
      {
        method: "PAYMENT_METHOD_CASH",
        tendered: money(currency(), chosen),
        applied_to_bill: money(currency(), total()),
        tip: money(currency(), tip()),
      },
    ]);
  };

  const payCard = () =>
    void pay([
      {
        method: "PAYMENT_METHOD_CARD",
        // A card takes the sale plus whatever tip was added — there is no note to choose and no
        // change to give, so the tendered amount is the whole of what the terminal will capture.
        tendered: money(currency(), total() + tip()),
        applied_to_bill: money(currency(), total()),
        tip: money(currency(), tip()),
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

            <details class="mt-6 rounded-token border border-line bg-surface px-3 py-2">
              <summary class="min-h-touch cursor-pointer text-sm font-semibold text-ink-muted">
                {t("pay.buyer")}
              </summary>
              <p class="mt-1 text-sm text-ink-muted">{t("pay.buyer_hint")}</p>
              <label class="mt-3 block text-sm">
                {t("pay.buyer_name")}
                <input
                  type="text"
                  class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                  value={buyerName()}
                  onInput={(event) => setBuyerName(event.currentTarget.value)}
                />
              </label>
              <label class="mt-2 block text-sm">
                {t("pay.buyer_tax_code")}
                <input
                  type="text"
                  class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                  value={buyerTaxCode()}
                  onInput={(event) => setBuyerTaxCode(event.currentTarget.value)}
                />
              </label>
              <label class="mt-2 block text-sm">
                {t("pay.buyer_address")}
                <input
                  type="text"
                  class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                  value={buyerAddress()}
                  onInput={(event) => setBuyerAddress(event.currentTarget.value)}
                />
              </label>
            </details>

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
                    {amount === total() ? t("pay.exact") : formatMoney(money(currency(), amount))}
                  </button>
                )}
              </For>
            </div>
            <p class="mt-2 text-sm text-ink-muted">
              {t("pay.change")}: <span class="tabular-nums">{formatMoney(money(currency(), change()))}</span>
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
              <p
                class="mt-1 text-sm"
                classList={{
                  "text-ink-muted":
                    bill().receipt_print === undefined || bill().receipt_print === "PRINTED",
                  "text-danger":
                    bill().receipt_print !== undefined && bill().receipt_print !== "PRINTED",
                }}
              >
                {t(receiptPrintKey(bill().receipt_print))}
              </p>
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
