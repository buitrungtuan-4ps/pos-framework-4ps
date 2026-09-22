import { For, Show, createSignal, onMount } from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";

import { ApiError } from "../api/client";
import type { BillResponse, BuyerRequest, CheckResponse, PaymentRequest } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { money, percentOf, quickCashFor, roundToIncrement } from "../lib/money";
import {
  applyDiscount,
  cashDenominations,
  cashRoundingIncrement,
  loadCheck,
  openBill,
  openBillFor,
  reasonsFor,
  settle,
  tenderAccepted,
  tipsEnabled,
  voidBill,
  formatAmount,
} from "../state/store";

// The action voiding a whole bill cites (ADR-0115). A separate action from voiding a line, so a
// store can hold reasons for one and not the other — and the picker here offers only the entries
// that declare this one.
const VOID_BILL = "REASON_ACTION_VOID_BILL";

// The action a discount cites. Its own action, like the void's: a store may publish "Goodwill" for
// knocking money off and not for cancelling a bill, and the edge refuses a reason published for
// the wrong one — so a picker that offered both would offer a refusal.
const DISCOUNT = "REASON_ACTION_DISCOUNT";

// The shares of the bill the tip row offers. Three, because the row has four columns and one of
// them is "none" — a fourth percentage would cost a line break on a phone for a choice a cashier
// makes by tapping the nearest of three.
const TIP_PERCENTS = [5, 10, 15] as const;

// Whether a set of tip keys is still worth offering: every key positive, and every key larger than
// the one before it.
//
// The guard on the cash snap below. On a bill of 8,000₫ a 1,000₫ increment turns 5/10/15% into 0,
// 1,000 and 1,000 — a dead button and a duplicate, on a row whose buttons carry an amount and
// nothing else, so a cashier cannot tell which is which or why one of them does nothing. The exact
// figures are less tidy and strictly more useful, so the snap stands down rather than degrading the
// row it was meant to improve.
function distinctAndSpendable(keys: readonly number[]): boolean {
  return keys.every((amount, index) => amount > (index === 0 ? 0 : (keys[index - 1] ?? 0)));
}

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
// task in `ui/scripts/step-tasks.mjs`.
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
    // A printer whose transport belongs to another device (ADR-0112). Three answers rather than
    // one, because they send a cashier to three different places: wait, go and look at the
    // terminal, or go and look at the printer.
    case "QUEUED_TO_AGENT":
      return "pay.print_queued";
    case "PRINT_AGENT_UNAVAILABLE":
      return "pay.print_agent_unavailable";
    case "PRINT_QUEUE_FULL":
      return "pay.print_queue_full";
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
  // Voiding this bill before it settles (ADR-0115, §6). Always a manager: a bill is money whether
  // or not the kitchen started, so unlike a line there is no shape of it that passes without one.
  // The badge and PIN live here for the length of the refusal and are cleared with the panel.
  const [voidingBill, setVoidingBill] = createSignal(false);
  const [voided, setVoided] = createSignal(false);
  const [approverCode, setApproverCode] = createSignal("");
  const [approverPin, setApproverPin] = createSignal("");
  // Taking money off before it settles. The manager fields above are shared with the void, which is
  // correct rather than lazy: only one of the two panels is ever open, and a cashier who typed a
  // badge for one and then changed their mind should not type it again for the other.
  const [discounting, setDiscounting] = createSignal(false);
  // What comes off, as typed. A string, not a number, because a half-typed "1" must stay "1" rather
  // than becoming a figure the screen then reformats under the operator's fingers.
  const [discountText, setDiscountText] = createSignal("");

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
  // scale with the check.
  //
  // `(total * percent) / 100` is what this line used to say, and it is a float division wearing an
  // integer's clothes. It was invisible for as long as every figure on the check was a round
  // thousand: a bill of 304,733₫ makes the 5% key 15,236.65, `Money.amount_minor` is an `i64`, and
  // the edge's deserializer refuses it — *"invalid type: floating point"*. What the cashier saw was
  // the generic store error, on the settle, with the guest's money already on the counter. Nothing
  // about the message said which of the four buttons they had pressed was the problem, and tapping
  // "no tip" made it go away, which is the kind of fix that becomes folklore.
  //
  // `percentOf` is the whole-minor-unit answer, rounded the way the edge rounds so the two agree.
  //
  // Whole is not yet spendable, which is the second half. 15,237₫ is an integer and still not an
  // amount a guest can leave: Vietnam's smallest note is 1,000 đồng, India's smallest coin is the
  // rupee. Where the store's country rounds its cash the keys round with it, so the button reads
  // 15,000₫ — what somebody would actually put on the table — and where it does not, Japan and the
  // United States among them, nothing is snapped and the exact share stands.
  //
  // The snap stands down on a small bill. Under about 20,000₫ a 1,000₫ increment swallows the gap
  // between 5% and 15% and the row collapses onto one amount; `distinctAndSpendable` catches that
  // and the exact figures are used instead. Better a key of 400₫ a cashier rounds in their head
  // than three identical buttons they cannot choose between.
  const tipKeys = () => {
    const exact = TIP_PERCENTS.map((percent) => percentOf(total(), percent));
    const increment = cashRoundingIncrement();
    if (increment === null) {
      return exact;
    }
    const snapped = exact.map((amount) => roundToIncrement(amount, increment));
    return distinctAndSpendable(snapped) ? snapped : exact;
  };

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

  const readyToVoid = () => approverCode().trim() !== "" && approverPin() !== "";

  const closeVoid = () => {
    setVoidingBill(false);
    setApproverCode("");
    setApproverPin("");
  };

  // The first tap: open the picker. Nothing is written until a reason is chosen — the reason is
  // mandatory, not a question asked afterwards.
  const askVoidBill = () => {
    setError(null);
    setVoidingBill(true);
  };

  // The second tap: void the bill, citing this reason and the manager standing at the till. The
  // panel stays open on a refusal — a settled bill is refused by the bill machine, and reading why
  // is the point.
  const voidBillReason = (reasonCodeId: string) => {
    const id = billId();
    if (id === null) {
      return;
    }
    setError(null);
    void voidBill(id, reasonCodeId, {
      approver_code: approverCode().trim(),
      approver_pin: approverPin(),
    })
      .then(() => {
        closeVoid();
        setVoided(true);
      })
      .catch((caught: unknown) =>
        setError(caught instanceof ApiError ? caught.message : t("common.store_error")),
      );
  };

  // Minor units, or null while what has been typed is not a number the till can send. Integer the
  // whole way (ADR-0028): the field takes digits and nothing else, so there is no decimal to lose.
  const discountAmount = () => {
    const typed = discountText().trim();
    if (typed === "" || !/^[0-9]+$/.test(typed)) {
      return null;
    }
    const amount = Number(typed);
    return amount > 0 ? amount : null;
  };

  // The reason buttons stay disabled until there is an amount and a manager, for the reason the
  // void's are: a button that can only produce a refusal teaches an operator the till is unreliable.
  const readyToDiscount = () =>
    discountAmount() !== null && approverCode().trim() !== "" && approverPin() !== "";

  const closeDiscount = () => {
    setDiscounting(false);
    setDiscountText("");
    setApproverCode("");
    setApproverPin("");
  };

  const askDiscount = () => {
    setError(null);
    setDiscounting(true);
  };

  // The second tap: apply it, citing this reason and the manager. The edge answers with the whole
  // re-priced bill and the screen takes that figure — it never subtracts the discount itself,
  // because the tax follows the reduced base and a second opinion about it is a second price.
  const discountReason = (reasonCodeId: string) => {
    const id = billId();
    const amount = discountAmount();
    if (id === null || amount === null) {
      return;
    }
    setError(null);
    void applyDiscount(id, money(currency(), amount), reasonCodeId, {
      approver_code: approverCode().trim(),
      approver_pin: approverPin(),
    })
      .then((totals) => {
        setCheck(totals);
        closeDiscount();
      })
      .catch((caught: unknown) =>
        setError(caught instanceof ApiError ? caught.message : t("common.store_error")),
      );
  };

  // What the screen becomes once the bill is voided: no money was taken, and the order is still
  // sitting on the table to be charged again. The way back is the order, not the floor — a voided
  // bill usually means the cashier is about to open the right one.
  const voidedPanel = () => (
    <div class="mt-6 rounded-token border border-line bg-surface p-4" data-outcome="bill-voided">
      <p class="text-lg font-semibold">{t("pay.void_done")}</p>
      <p class="mt-1 text-sm text-ink-muted">{t("pay.void_done_hint")}</p>
      <button
        type="button"
        class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
        onClick={() => navigate(`/table/${params.id}`)}
      >
        {t("common.back_order")}
      </button>
    </div>
  );

  return (
    <section class="mx-auto max-w-xl p-4">
      <a href={`/table/${params.id}`} class="text-sm text-ink-muted no-underline">
        {t("common.back_order")}
      </a>

      <Show
        when={done()}
        fallback={
          <Show when={!voided()} fallback={voidedPanel()}>
            <p class="mt-4 text-sm text-ink-muted">{t("pay.amount_due")}</p>
            <Show when={check()} fallback={<p class="text-2xl font-semibold tabular-nums">{"—"}</p>}>
              {(totals) => (
                <>
                  {/* A reduced bill says so and by how much. A total that simply came out smaller
                      than the food on the table is the kind of figure a guest queries and nobody
                      at the till can explain. Shown only when there is one, because a row of
                      zeroes on every bill is noise the eye learns to skip. */}
                  <Show when={totals().discount_total.amount_minor > 0}>
                    <p
                      class="flex justify-between text-sm text-ink-muted"
                      data-outcome="bill-discounted"
                    >
                      <span>{t("pay.discount_applied")}</span>
                      <span class="tabular-nums">
                        {"− "}
                        {formatAmount(totals().discount_total)}
                      </span>
                    </p>
                  </Show>
                  <Show when={totals().comp_total.amount_minor > 0}>
                    <p class="flex justify-between text-sm text-ink-muted">
                      <span>{t("pay.comp_applied")}</span>
                      <span class="tabular-nums">
                        {"− "}
                        {formatAmount(totals().comp_total)}
                      </span>
                    </p>
                  </Show>
                  <p class="text-2xl font-semibold tabular-nums">
                    {formatAmount(totals().total_due)}
                  </p>
                </>
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
                      {formatAmount(money(currency(), amount))}
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
                    {amount === total() ? t("pay.exact") : formatAmount(money(currency(), amount))}
                  </button>
                )}
              </For>
            </div>
            <p class="mt-2 text-sm text-ink-muted">
              {t("pay.change")}: <span class="tabular-nums">{formatAmount(money(currency(), change()))}</span>
            </p>
            </Show>

            <div class="mt-4 flex flex-col gap-2">
              <Show when={tenderAccepted("PAYMENT_METHOD_CASH")}>
              <button
                type="button"
                class="min-h-money rounded-token bg-primary text-lg font-semibold text-primary-ink"
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

            {/*
              Voiding the bill (ADR-0115, §6). Secondary to the tender buttons on purpose: it is the
              rare act, and it is the one that needs a second person. Always a manager — a bill is
              money whether or not the kitchen started — so unlike a line there is no shape of this
              the till lets through on its own.

              The reason buttons stay disabled until the manager's badge and PIN are filled, which
              keeps the void at the three taps §6 allows a rare action (pay, void, reason) rather
              than spending one on a request that could only come back `403`.
            */}
            {/*
              Taking money off (roadmap B2.2). Three taps from the order screen — pay, discount,
              reason — which is §6's ceiling for a rare action and the same shape the bill void has.
              The amount and the manager's badge are **typed**, and typing is not a tap, exactly as
              the shift float and the void's PIN are not.

              The manager is not this screen's rule. `billing.discount.apply` is granted to a server
              and carries no PIN flag — but no store publishes the discount ceiling that permission's
              own description refers to, so the edge reads the ceiling as zero and answers `403`
              naming `billing.discount.override_ceiling`. The fields are here because that is the
              answer today; when a store publishes a ceiling, a small discount will start going
              through and this panel will not have to change.
            */}
            <Show
              when={discounting()}
              fallback={
                <button
                  type="button"
                  class="mt-6 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50"
                  disabled={billId() === null || reasonsFor(DISCOUNT).length === 0}
                  data-step="askDiscount"
                  onClick={() => askDiscount()}
                >
                  {t("pay.discount")}
                </button>
              }
            >
              <div class="mt-6 rounded-token border border-line bg-surface p-3">
                <h2 class="font-semibold">{t("pay.discount_title")}</h2>
                <label class="mt-2 block text-sm">
                  {t("pay.discount_amount")}
                  <input
                    id="discount-amount"
                    type="text"
                    inputmode="numeric"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2 tabular-nums"
                    value={discountText()}
                    onInput={(event) => setDiscountText(event.currentTarget.value)}
                  />
                </label>
                <p class="mt-2 text-sm text-ink-muted">{t("pay.discount_manager")}</p>
                <label class="mt-2 block text-sm">
                  {t("pay.approver_code")}
                  <input
                    type="text"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                    value={approverCode()}
                    onInput={(event) => setApproverCode(event.currentTarget.value)}
                  />
                </label>
                <label class="mt-2 block text-sm">
                  {t("pay.approver_pin")}
                  <input
                    type="password"
                    inputmode="numeric"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                    value={approverPin()}
                    onInput={(event) => setApproverPin(event.currentTarget.value)}
                  />
                </label>
                <p class="mt-3 text-sm text-ink-muted">{t("pay.discount_reason")}</p>
                <div class="mt-2 grid grid-cols-2 gap-2">
                  <For each={reasonsFor(DISCOUNT)}>
                    {(reason) => (
                      <button
                        type="button"
                        class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                        disabled={!readyToDiscount()}
                        data-step="discountReason"
                        onClick={() => discountReason(reason.reason_code_id)}
                      >
                        {reason.display_name}
                      </button>
                    )}
                  </For>
                </div>
                <button
                  type="button"
                  class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
                  onClick={() => closeDiscount()}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </Show>

            <Show
              when={voidingBill()}
              fallback={
                <button
                  type="button"
                  class="mt-6 min-h-touch w-full rounded-token border border-line text-sm text-ink-muted disabled:opacity-50"
                  disabled={billId() === null || reasonsFor(VOID_BILL).length === 0}
                  data-step="askVoidBill"
                  onClick={() => askVoidBill()}
                >
                  {t("pay.void")}
                </button>
              }
            >
              <div class="mt-6 rounded-token border border-line bg-surface p-3">
                <h2 class="font-semibold">{t("pay.void_title")}</h2>
                <p class="mt-1 text-sm text-ink-muted">{t("pay.void_manager")}</p>
                <label class="mt-2 block text-sm">
                  {t("pay.approver_code")}
                  <input
                    type="text"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                    value={approverCode()}
                    onInput={(event) => setApproverCode(event.currentTarget.value)}
                  />
                </label>
                <label class="mt-2 block text-sm">
                  {t("pay.approver_pin")}
                  <input
                    type="password"
                    inputmode="numeric"
                    class="mt-1 min-h-touch w-full rounded-token border border-line bg-surface px-2"
                    value={approverPin()}
                    onInput={(event) => setApproverPin(event.currentTarget.value)}
                  />
                </label>
                <p class="mt-3 text-sm text-ink-muted">{t("pay.void_reason")}</p>
                <div class="mt-2 grid grid-cols-2 gap-2">
                  <For each={reasonsFor(VOID_BILL)}>
                    {(reason) => (
                      <button
                        type="button"
                        class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                        disabled={!readyToVoid()}
                        data-step="voidBillReason"
                        onClick={() => voidBillReason(reason.reason_code_id)}
                      >
                        {reason.display_name}
                      </button>
                    )}
                  </For>
                </div>
                <button
                  type="button"
                  class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
                  onClick={() => closeVoid()}
                >
                  {t("common.cancel")}
                </button>
              </div>
            </Show>
          </Show>
        }
      >
        {(bill) => (
          // The settled block: what a pay flow has to reach, and what an undeclared extra tap would
          // stop it reaching (ADR-0109).
          <div class="mt-6 rounded-token border border-line bg-surface p-4" data-outcome="settled">
            <p class="text-lg font-semibold text-ok">{t("pay.settled")}</p>
            <p class="mt-2 tabular-nums">
              {t("pay.receipt", { number: bill().receipt_number ?? 0 })}
            </p>
            <p class="mt-1 text-ink-muted">
              {t("pay.change")}: <span class="tabular-nums">{formatAmount(money(currency(), change()))}</span>
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
              class="mt-4 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
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
