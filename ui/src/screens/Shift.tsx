import { For, Show, createSignal, onMount } from "solid-js";

import { ApproverFields } from "../components/ApproverFields";
import { Keypad } from "../components/Keypad";
import { PageHeader } from "../components/ui";
import type { DrawerOutcome } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { money, type Money } from "../lib/money";
import {
  closeShift,
  countShift,
  currencyExponent,
  formatAmount,
  loadShift,
  openDrawerNoSale,
  openShift,
  parseAmount,
  reasonsFor,
  recordCashMovement,
  state,
  storeCurrency,
} from "../state/store";
import { openingFloatMinor } from "../state/session";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { asksApprover, can } from "../state/permissions";

// The reason list's three acts for the drawer (ADR-0115), as `/api/reason-codes` spells them.
const PAID_IN = "REASON_ACTION_CASH_PAID_IN";
const PAID_OUT = "REASON_ACTION_CASH_PAID_OUT";
const DRAWER_OPEN = "REASON_ACTION_DRAWER_OPEN";

// What to tell the cashier about the drawer after an act that opened it (ADR-0165). A drawer that
// should have sprung and did not is said in red, because cash is waiting to go in or come out; a
// till with no drawer connected says to use the key, quietly, because that is its ordinary state.
function drawerKey(outcome: DrawerOutcome): MessageKey {
  switch (outcome) {
    case "OPENED":
      return "shift.drawer_opened";
    case "NO_DRAWER":
      return "shift.drawer_no_drawer";
    default:
      return "pay.drawer_unavailable";
  }
}

// Whether a total is worth a line: nothing paid in or out shows nothing, so a shift that moved no
// cash reads as it always did.
const nonZero = (amount: Money | undefined) => amount !== undefined && amount.amount_minor !== 0;

// The cash shift: open with a float, pay cash in and out, enter the blind count (the screen shows
// nothing about what is expected), then close to reveal the variance. The blindness is the control
// — counting before the expectation is shown (§11.1) — so the count field never sits beside an
// expected figure. What was paid in and out is shown, because the cashier entered it: it says
// nothing about the cash taken on bills, which is the part the close keeps to itself.
//
// Two of the store's settings change that (ADR-0160 decision 2). `shift.opening_float_minor` fills
// in the float, which the cashier may change before opening. `shift.blind_close` off has the edge
// send what the drawer should hold, and then, and only then, the count field shows it.
export function Shift() {
  const [amount, setAmount] = createSignal("");
  // The float field holds the store's float until the cashier types, and what they typed from then
  // on, an emptied field included. Set back after each act, so the next opening fills it in again.
  const [floatTyped, setFloatTyped] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  // A paid in or out (ADR-0165 decision 1): the amount is typed first, then **Paid in** or **Paid
  // out** says which way, then a reason records it. Two taps after the number.
  const [movementAmount, setMovementAmount] = createSignal("");
  const [movementPad, setMovementPad] = createSignal(false);
  const [direction, setDirection] = createSignal<"in" | "out" | null>(null);
  const [moved, setMoved] = createSignal<{ key: MessageKey; amount: Money } | null>(null);
  // What came of the drawer for that movement, said beside it rather than under the no-sale button.
  const [movementDrawer, setMovementDrawer] = createSignal<DrawerOutcome | null>(null);
  // A drawer opened without a sale (decision 3): a manager's code and PIN, then the reason.
  const [openingDrawer, setOpeningDrawer] = createSignal(false);
  const [approverCode, setApproverCode] = createSignal("");
  const [approverPin, setApproverPin] = createSignal("");
  const [drawer, setDrawer] = createSignal<DrawerOutcome | null>(null);

  const shift = () => state.shift;
  const phase = () => shift()?.state ?? "NONE";

  // What the drawer should hold, as the edge reports it to a store whose count is not blind. Read
  // again when the screen opens on a shift still trading, so it includes the cash taken since the
  // shift was last read. A closed shift is left as it is, so its variance stays on screen.
  onMount(() => {
    const current = state.shift;
    if (current !== null && current.state !== "SHIFT_STATE_CLOSED") {
      void loadShift();
    }
  });

  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      setAmount("");
      setFloatTyped(false);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  // The store's float as the cashier would type it: whole units of the currency, which is all the
  // keypad types. A store that sets none fills in nothing, as before the setting, and so does a
  // float that is not a whole number of units, rather than open the shift on a different amount.
  const storeFloat = () => {
    const minor = openingFloatMinor();
    const unit = 10 ** currencyExponent();
    return minor > 0 && minor % unit === 0 ? String(minor / unit) : "";
  };
  const floatText = () => (floatTyped() ? amount() : storeFloat());
  const typeFloat = (text: string) => {
    setFloatTyped(true);
    setAmount(text);
  };
  const floatParsed = () => parseAmount(floatText());

  // Parsed in the store's own currency, not a literal (roadmap E5): the minor-unit scale differs
  // between currencies, so parsing a typed figure as VND on a two-decimal currency would be out by
  // a factor of a hundred on the store's own cash count.
  const parsed = () => parseAmount(amount());
  const movementParsed = () => {
    const value = parseAmount(movementAmount());
    return value !== null && value > 0 ? value : null;
  };

  const recordMovement = async (way: "in" | "out", reasonCodeId: string) => {
    const current = shift();
    const value = movementParsed();
    if (!current || value === null) {
      return;
    }
    setError(null);
    try {
      const outcome = await recordCashMovement(current.shiftId, way, value, reasonCodeId);
      setMoved({
        key: way === "in" ? "shift.paid_in_recorded" : "shift.paid_out_recorded",
        amount: money(storeCurrency(), value),
      });
      setMovementDrawer(outcome ?? null);
      setDirection(null);
      setMovementAmount("");
      setMovementPad(false);
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  const closeDrawerPanel = () => {
    setOpeningDrawer(false);
    setApproverCode("");
    setApproverPin("");
  };

  // A manager's code and PIN, where the person needs an approver for the drawer: everybody, where
  // the store does not enforce each person's own set (ADR-0158).
  const drawerAsks = () => asksApprover("cash.drawer.open_no_sale");
  const openDrawer = async (reasonCodeId: string) => {
    setError(null);
    const approval = drawerAsks()
      ? { approver_code: approverCode(), approver_pin: approverPin() }
      : undefined;
    try {
      setDrawer(await openDrawerNoSale(reasonCodeId, approval));
      closeDrawerPanel();
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  const approverReady = () =>
    !drawerAsks() || (approverCode().trim() !== "" && approverPin().trim() !== "");

  // The taps the step budget counts, named for what the operator is doing (`ui/scripts/step-budget.mjs`
  // resolves a declared step to the handler its element calls).
  const askPaidIn = () => setDirection("in");
  const askPaidOut = () => setDirection("out");
  const movementReason = (reasonCodeId: string) => {
    const way = direction();
    if (way !== null) {
      void recordMovement(way, reasonCodeId);
    }
  };
  const askOpenDrawer = () => {
    setDrawer(null);
    setOpeningDrawer(true);
  };
  const drawerReason = (reasonCodeId: string) => void openDrawer(reasonCodeId);

  return (
    <section class="mx-auto max-w-md p-4">
      <PageHeader title={t("shift.title")} />

      <Show when={error()}>
        {(message) => (
          <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>

      <Show
        when={(phase() === "NONE" || phase() === "SHIFT_STATE_CLOSED") && can("cash.shift.open")}
      >
        <label class="block text-sm text-ink-muted" for="float">
          {t("shift.float_label", { currency: storeCurrency() })}
        </label>
        <input
          id="float"
          inputmode="numeric"
          class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
          value={floatText()}
          onInput={(event) => typeFloat(event.currentTarget.value)}
        />
        <Keypad value={floatText()} onChange={typeFloat} data-step="floatKeypad" />
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
          disabled={floatParsed() === null}
          data-step="openShift"
          onClick={() => {
            const value = floatParsed();
            if (value !== null) {
              void run(() => openShift(value));
            }
          }}
        >
          {t("shift.open")}
        </button>
      </Show>

      <Show when={phase() === "SHIFT_STATE_OPEN"}>
        <p class="text-ink-muted" data-outcome="shift-open">
          {t(shift()?.expected ? "shift.open_hint_not_blind" : "shift.open_hint")}
        </p>

        {/*
          Cash in and out of the drawer outside a sale (ADR-0165). The amount comes first, so the
          two taps after it are the whole act: which way, and why.
        */}
        <Show when={can("cash.movement.record")}>
        <div class="mt-4 rounded-token border border-line bg-surface p-3">
          <h2 class="font-semibold">{t("shift.cash_title")}</h2>
          <label class="mt-2 block text-sm text-ink-muted" for="movement-amount">
            {t("shift.cash_amount_label", { currency: storeCurrency() })}
          </label>
          <input
            id="movement-amount"
            inputmode="numeric"
            class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
            value={movementAmount()}
            onFocus={() => setMovementPad(true)}
            onInput={(event) => setMovementAmount(event.currentTarget.value)}
          />
          <Show when={movementPad()}>
            <Keypad value={movementAmount()} onChange={setMovementAmount} data-step="movementKeypad" />
          </Show>
          <div class="mt-3 grid grid-cols-2 gap-2">
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface font-semibold disabled:opacity-50"
              classList={{ "border-primary": direction() === "in" }}
              disabled={movementParsed() === null || reasonsFor(PAID_IN).length === 0}
              data-step="askPaidIn"
              onClick={() => askPaidIn()}
            >
              {t("shift.paid_in")}
            </button>
            <button
              type="button"
              class="min-h-touch rounded-token border border-line bg-surface font-semibold disabled:opacity-50"
              classList={{ "border-primary": direction() === "out" }}
              disabled={movementParsed() === null || reasonsFor(PAID_OUT).length === 0}
              data-step="askPaidOut"
              onClick={() => askPaidOut()}
            >
              {t("shift.paid_out")}
            </button>
          </div>
          <Show when={direction()}>
            {(way) => (
              <>
                <p class="mt-3 text-sm text-ink-muted">
                  {t(way() === "in" ? "shift.paid_in_reason" : "shift.paid_out_reason")}
                </p>
                <div class="mt-2 grid grid-cols-2 gap-2">
                  <For each={reasonsFor(way() === "in" ? PAID_IN : PAID_OUT)}>
                    {(reason) => (
                      <button
                        type="button"
                        class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                        disabled={movementParsed() === null}
                        data-step="movementReason"
                        onClick={() => movementReason(reason.reason_code_id)}
                      >
                        {reason.display_name}
                      </button>
                    )}
                  </For>
                </div>
                <button
                  type="button"
                  class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
                  onClick={() => setDirection(null)}
                >
                  {t("common.cancel")}
                </button>
              </>
            )}
          </Show>
          <Show when={moved()}>
            {(done) => (
              <p class="mt-3 text-sm" role="status" data-outcome="cash-moved">
                {t(done().key, { amount: formatAmount(done().amount) })}
              </p>
            )}
          </Show>
          <Show when={movementDrawer()}>
            {(outcome) => (
              <p
                class="mt-1 text-sm"
                classList={{
                  "text-ink-muted": outcome() !== "DRAWER_UNAVAILABLE",
                  "text-danger": outcome() === "DRAWER_UNAVAILABLE",
                }}
                role="status"
                data-outcome="movement-drawer"
              >
                {t(drawerKey(outcome()))}
              </p>
            )}
          </Show>
          <Show when={nonZero(shift()?.paidIn) || nonZero(shift()?.paidOut)}>
            <p class="mt-2 text-sm text-ink-muted tabular-nums" data-outcome="cash-totals">
              {t("shift.cash_totals", {
                paid_in: formatAmount(shift()?.paidIn ?? money(storeCurrency(), 0)),
                paid_out: formatAmount(shift()?.paidOut ?? money(storeCurrency(), 0)),
              })}
            </p>
          </Show>
        </div>
        </Show>

        <Show when={can("cash.shift.close")}>
        <label class="mt-4 block text-sm text-ink-muted" for="count">
          {t("shift.count_label", { currency: storeCurrency() })}
        </label>
        {/* Only where the store's count is not blind: a blind store is sent no expectation. */}
        <Show when={shift()?.expected}>
          {(expected) => (
            <p class="mt-1 text-sm tabular-nums" data-outcome="shift-expected">
              {t("shift.expected_now", { amount: formatAmount(expected()) })}
            </p>
          )}
        </Show>
        <input
          id="count"
          inputmode="numeric"
          class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
          value={amount()}
          onInput={(event) => setAmount(event.currentTarget.value)}
        />
        <Keypad value={amount()} onChange={setAmount} data-step="countKeypad" />
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
          disabled={parsed() === null}
          data-step="countShift"
          onClick={() => {
            const value = parsed();
            const current = shift();
            if (value !== null && current) {
              void run(() => countShift(current.shiftId, value));
            }
          }}
        >
          {t("shift.enter_count")}
        </button>
        </Show>
      </Show>

      <Show when={phase() === "SHIFT_STATE_COUNTED"}>
        <p class="text-ink-muted" data-outcome="shift-counted">
          {t("shift.counted_hint")}
        </p>
        <Show when={can("cash.shift.close")}>
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink"
          data-step="closeShift"
          onClick={() => {
            const current = shift();
            if (current) {
              void run(() => closeShift(current.shiftId));
            }
          }}
        >
          {t("shift.close")}
        </button>
        </Show>
      </Show>

      <Show when={phase() === "SHIFT_STATE_CLOSED" && shift()?.variance}>
        {(variance) => (
          <div class="mt-4 rounded-token border border-line bg-surface p-4" data-outcome="shift-closed">
            <p class="font-semibold">{t("shift.closed")}</p>
            <Show when={nonZero(shift()?.paidIn)}>
              <p class="mt-2 text-ink-muted">
                {t("shift.paid_in")}{" "}
                <span class="tabular-nums">{formatAmount(shift()?.paidIn ?? money(storeCurrency(), 0))}</span>
              </p>
            </Show>
            <Show when={nonZero(shift()?.paidOut)}>
              <p class="text-ink-muted">
                {t("shift.paid_out")}{" "}
                <span class="tabular-nums">{formatAmount(shift()?.paidOut ?? money(storeCurrency(), 0))}</span>
              </p>
            </Show>
            <p class="mt-2 text-ink-muted">
              {t("shift.expected")}{" "}
              <span class="tabular-nums">{formatAmount(shift()?.expected ?? money(storeCurrency(), 0))}</span>
            </p>
            <p class="text-ink-muted">
              {t("shift.counted")}{" "}
              <span class="tabular-nums">{formatAmount(shift()?.counted ?? money(storeCurrency(), 0))}</span>
            </p>
            <p
              classList={{
                "text-danger": variance().amount_minor !== 0,
                "text-ok": variance().amount_minor === 0,
              }}
            >
              {t("shift.variance")} <span class="tabular-nums">{formatAmount(variance())}</span>
            </p>
            <Show when={shift()?.reportPrint}>
              {(outcome) => (
                <p class="mt-2 text-sm text-ink-muted" role="status" data-outcome="shift-report-print">
                  {t(printOutcomeKey(outcome(), "shift.report_printed"))}
                </p>
              )}
            </Show>
          </div>
        )}
      </Show>

      {/*
        The drawer opened without a sale (ADR-0165 decision 3), in every state of the shift: it moves
        no cash, so it needs no open shift. A manager's code and PIN, then the reason.
      */}
      <div class="mt-6">
        <Show
          when={openingDrawer()}
          fallback={
            <Show when={can("cash.drawer.open_no_sale")}>
            <button
              type="button"
              class="min-h-touch w-full rounded-token border border-line text-sm disabled:opacity-50"
              disabled={reasonsFor(DRAWER_OPEN).length === 0}
              data-step="askOpenDrawer"
              onClick={() => askOpenDrawer()}
            >
              {t("shift.open_drawer")}
            </button>
            </Show>
          }
        >
          <div class="rounded-token border border-line bg-surface p-3">
            <h2 class="font-semibold">{t("shift.open_drawer_title")}</h2>
            <Show when={drawerAsks()}>
            <p class="mt-1 text-sm text-ink-muted">{t("shift.open_drawer_manager")}</p>
            <ApproverFields
              id="no-sale-approver"
              code={approverCode()}
              pin={approverPin()}
              onCode={setApproverCode}
              onPin={setApproverPin}
              codeLabel={t("shift.approver_code")}
              pinLabel={t("shift.approver_pin")}
            />
            </Show>
            <p class="mt-3 text-sm text-ink-muted">{t("shift.open_drawer_reason")}</p>
            <div class="mt-2 grid grid-cols-2 gap-2">
              <For each={reasonsFor(DRAWER_OPEN)}>
                {(reason) => (
                  <button
                    type="button"
                    class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                    disabled={!approverReady()}
                    data-step="drawerReason"
                    onClick={() => drawerReason(reason.reason_code_id)}
                  >
                    {reason.display_name}
                  </button>
                )}
              </For>
            </div>
            <button
              type="button"
              class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
              onClick={() => closeDrawerPanel()}
            >
              {t("common.cancel")}
            </button>
          </div>
        </Show>
        <Show when={drawer()}>
          {(outcome) => (
            <p
              class="mt-2 text-sm"
              classList={{
                "text-ink-muted": outcome() !== "DRAWER_UNAVAILABLE",
                "text-danger": outcome() === "DRAWER_UNAVAILABLE",
              }}
              role="status"
              data-outcome="drawer-opened"
            >
              {t(drawerKey(outcome()))}
            </p>
          )}
        </Show>
      </div>
    </section>
  );
}
