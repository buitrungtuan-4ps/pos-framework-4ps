import { For, Show, createSignal } from "solid-js";

import { ApproverFields } from "../components/ApproverFields";
import { Keypad } from "../components/Keypad";
import { PageHeader } from "../components/ui";
import type { DrawerOutcome } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { money, type Money } from "../lib/money";
import {
  closeShift,
  countShift,
  formatAmount,
  openDrawerNoSale,
  openShift,
  parseAmount,
  reasonsFor,
  recordCashMovement,
  state,
  storeCurrency,
} from "../state/store";
import { errorMessage } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";

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
export function Shift() {
  const [amount, setAmount] = createSignal("");
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

  const run = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
      setAmount("");
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

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

  const openDrawer = async (reasonCodeId: string) => {
    setError(null);
    try {
      setDrawer(await openDrawerNoSale(reasonCodeId, approverCode(), approverPin()));
      closeDrawerPanel();
    } catch (caught) {
      setError(errorMessage(caught));
    }
  };

  const approverReady = () => approverCode().trim() !== "" && approverPin().trim() !== "";

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

      <Show when={phase() === "NONE" || phase() === "SHIFT_STATE_CLOSED"}>
        <label class="block text-sm text-ink-muted" for="float">
          {t("shift.float_label", { currency: storeCurrency() })}
        </label>
        <input
          id="float"
          inputmode="numeric"
          class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
          value={amount()}
          onInput={(event) => setAmount(event.currentTarget.value)}
        />
        <Keypad value={amount()} onChange={setAmount} data-step="floatKeypad" />
        <button
          type="button"
          class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
          disabled={parsed() === null}
          data-step="openShift"
          onClick={() => {
            const value = parsed();
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
          {t("shift.open_hint")}
        </p>

        {/*
          Cash in and out of the drawer outside a sale (ADR-0165). The amount comes first, so the
          two taps after it are the whole act: which way, and why.
        */}
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

        <label class="mt-4 block text-sm text-ink-muted" for="count">
          {t("shift.count_label", { currency: storeCurrency() })}
        </label>
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

      <Show when={phase() === "SHIFT_STATE_COUNTED"}>
        <p class="text-ink-muted" data-outcome="shift-counted">
          {t("shift.counted_hint")}
        </p>
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
            <button
              type="button"
              class="min-h-touch w-full rounded-token border border-line text-sm disabled:opacity-50"
              disabled={reasonsFor(DRAWER_OPEN).length === 0}
              data-step="askOpenDrawer"
              onClick={() => askOpenDrawer()}
            >
              {t("shift.open_drawer")}
            </button>
          }
        >
          <div class="rounded-token border border-line bg-surface p-3">
            <h2 class="font-semibold">{t("shift.open_drawer_title")}</h2>
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
