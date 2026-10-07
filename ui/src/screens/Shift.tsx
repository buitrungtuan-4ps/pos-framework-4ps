import { For, Show, createSignal, onMount } from "solid-js";

import { ApproverFields } from "../components/ApproverFields";
import { Keypad } from "../components/Keypad";
import { PageHeader } from "../components/ui";
import type { ApproverRequest, DrawerEntry, DrawerOutcome, ShiftResponse } from "../api/types";
import { t, type MessageKey } from "../i18n";
import { money, type Money } from "../lib/money";
import {
  closeDrawerShifts,
  closeShift,
  countDrawerShift,
  countShift,
  currencyExponent,
  drawerPerTill,
  formatAmount,
  loadDrawers,
  loadShift,
  openDrawerNoSale,
  openDrawerShift,
  openShift,
  ownTill,
  parseAmount,
  reasonsFor,
  recordCashMovement,
  state,
  storeCurrency,
} from "../state/store";
import { openingFloatMinor } from "../state/session";
import { errorMessage, owedVariances } from "../lib/errors";
import { printOutcomeKey } from "../lib/print";
import { asksApprover, can } from "../state/permissions";

// The reason list's three acts for the drawer (ADR-0115), as `/api/reason-codes` spells them.
const PAID_IN = "REASON_ACTION_CASH_PAID_IN";
const PAID_OUT = "REASON_ACTION_CASH_PAID_OUT";
const DRAWER_OPEN = "REASON_ACTION_DRAWER_OPEN";
// …and the close's, where the store asks a reason of a large over or short (ADR-0167 decision 12).
const CASH_VARIANCE = "REASON_ACTION_CASH_VARIANCE";

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

// The refusals an act on a drawer in the list words for itself (ADR-0167): the till's one sentence
// for each speaks of a table or of this device, where it is the drawer picked that changed.
const DRAWER_REASONS: Readonly<Record<string, MessageKey>> = {
  TRANSITION_REFUSED: "error.drawer_changed",
  SHIFT_ALREADY_OPEN: "error.drawer_already_open",
  NOT_A_TILL: "error.drawer_not_a_till",
};

// Where a drawer in the list stands: closed, its shift open, or counted and waiting to close.
type DrawerPhase = "closed" | "open" | "counted";

function drawerPhase(drawer: DrawerEntry): DrawerPhase {
  if (drawer.shift === null) {
    return "closed";
  }
  return drawer.shift.state === "SHIFT_STATE_COUNTED" ? "counted" : "open";
}

// A drawer's handle in the list: its till. The store's one drawer has none, and is never listed.
const tillKey = (drawer: DrawerEntry) => drawer.terminal_device_id ?? "";

// A drawer's till by name, or one the store no longer lists, whose shift is still open.
const drawerName = (drawer: DrawerEntry) => drawer.name ?? t("shift.drawer_unlisted");

// What a drawer closed from the list came to, by its till's name.
interface ClosedDrawer {
  name: string;
  shift: ShiftResponse;
}

// The cash shift: open with a float, pay cash in and out, enter the blind count (the screen shows
// nothing about what is expected), then close to reveal the variance. The blindness is the control
// — counting before the expectation is shown (§11.1) — so the count field never sits beside an
// expected figure. What was paid in and out is shown, because the cashier entered it: it says
// nothing about the cash taken on bills, which is the part the close keeps to itself.
//
// Two of the store's settings change that (ADR-0160 decision 2). `shift.opening_float_minor` fills
// in the float, which the cashier may change before opening. `shift.blind_close` off has the edge
// send what the drawer should hold, and then, and only then, the count field shows it.
//
// Where the store keeps a drawer per till (ADR-0167), all of that is this till's own drawer, which
// comes first, on its till's own float. A device that is no till says so and offers no drawer of
// its own. Below it, for a person who may act on another till's drawer, every till's drawer: start
// one or several, count one, and close one or several together, each with a manager's code and PIN
// where the person needs one. What the store waits to change to, and a drawer open past its
// business day, are said wherever they apply.
//
// A close over or short by more than the store allows (`shift.variance_reason_minor`, ADR-0167
// decision 12) is answered with the variance its count fixed and the store's reasons for one, and a
// reason closes it. The count is final by then, so this shows what the close reveals anyway.
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
  // This till's own close, refused until it gives a reason for its over or short: its figures.
  const [owedClose, setOwedClose] = createSignal<ShiftResponse | null>(null);

  const shift = () => state.shift;
  const phase = () => shift()?.state ?? "NONE";

  // Every drawer the store keeps, and which of them is this device's, where it keeps one per till.
  const drawers = () => state.drawers?.drawers ?? [];
  const ownDrawer = () => {
    const own = ownTill();
    return own === undefined
      ? undefined
      : drawers().find((drawer) => drawer.terminal_device_id === own);
  };
  // A device that is no till has no drawer of its own (decision 6).
  const noTill = () => drawerPerTill() && ownTill() === undefined;
  // Every till's drawer, for a person who may act on another till's, directly or with a manager's
  // approval (decision 3; ADR-0158 decision 6, the till hides what a person cannot do).
  const listShown = () => drawerPerTill() && can("cash.shift.manage_other_till");
  // The model the store has published while a drawer still open keeps the other (decision 8).
  const waiting = () => state.drawers?.waiting_drawer_model;

  // What the drawer should hold, as the edge reports it to a store whose count is not blind. Read
  // again when the screen opens on a shift still trading, so it includes the cash taken since the
  // shift was last read. A closed shift is left as it is, so its variance stays on screen.
  onMount(() => {
    const current = state.shift;
    if (current !== null && current.state !== "SHIFT_STATE_CLOSED") {
      void loadShift();
    }
    void loadDrawers();
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

  // A float as the cashier would type it: whole units of the currency, which is all the keypad
  // types. Nothing fills in for no float, as before the setting, nor for one that is not a whole
  // number of units, rather than open the shift on a different amount.
  const wholeUnits = (minor: number) => {
    const unit = 10 ** currencyExponent();
    return minor > 0 && minor % unit === 0 ? String(minor / unit) : "";
  };
  // The store's float, or where the store keeps a drawer per till this till's own (decision 4).
  const storeFloat = () =>
    wholeUnits(ownDrawer()?.default_float.amount_minor ?? openingFloatMinor());
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

  // Closes this till's own drawer; where the store asks a reason first, says so with the figures.
  const closeOwn = (close: () => Promise<unknown>) =>
    run(async () => {
      try {
        await close();
        setOwedClose(null);
      } catch (caught) {
        const [owed] = owedVariances(caught) ?? [];
        if (owed === undefined) {
          throw caught;
        }
        setOwedClose(owed);
      }
    });
  const varianceReason = (reasonCodeId: string) => {
    const current = shift();
    if (current) {
      void closeOwn(() => closeShift(current.shiftId, reasonCodeId));
    }
  };
  // Only for the shift it was asked of: one closed meanwhile from the list asks nothing more.
  const owedHere = () => {
    const owed = owedClose();
    return owed !== null && owed.shift_id === shift()?.shiftId ? owed : null;
  };

  // ---- every till's drawer (ADR-0167 decision 3) ----
  //
  // Which drawers are picked, the float or count typed for one, the approver for another till's,
  // and what the last act on them came to.
  const [picked, setPicked] = createSignal<readonly string[]>([]);
  const [listAmount, setListAmount] = createSignal("");
  const [listFloatTyped, setListFloatTyped] = createSignal(false);
  const [listCode, setListCode] = createSignal("");
  const [listPin, setListPin] = createSignal("");
  const [listBusy, setListBusy] = createSignal(false);
  const [listError, setListError] = createSignal<string | null>(null);
  const [startedDrawers, setStartedDrawers] = createSignal<{
    started: string[];
    missed: string[];
    refusal: string | null;
  } | null>(null);
  const [countedDrawer, setCountedDrawer] = createSignal<string | null>(null);
  const [closedDrawers, setClosedDrawers] = createSignal<{
    drawers: ClosedDrawer[];
    slip?: string;
  } | null>(null);
  // The drawers a close was refused for until each has a reason, and the reasons given so far.
  const [owedDrawers, setOwedDrawers] = createSignal<readonly ShiftResponse[]>([]);
  const [givenReasons, setGivenReasons] = createSignal<Readonly<Record<string, string>>>({});

  // The drawers picked, as the list reads now, all in one state: what the list offers is what that
  // state allows. A drawer the list no longer has, or one that moved on, drops out.
  const pickedDrawers = () => {
    const chosen = drawers().filter((drawer) => picked().includes(tillKey(drawer)));
    const [first] = chosen;
    return first === undefined
      ? []
      : chosen.filter((drawer) => drawerPhase(drawer) === drawerPhase(first));
  };
  const pickedPhase = (): DrawerPhase | null => {
    const [first] = pickedDrawers();
    return first === undefined ? null : drawerPhase(first);
  };

  // Another till's drawer also takes `cash.shift.manage_other_till`, which a person may hold only
  // with a manager's approval: their code and PIN, for this one act, dropped after it.
  const listAsks = () =>
    pickedDrawers().some((drawer) => drawer.terminal_device_id !== ownTill()) &&
    asksApprover("cash.shift.manage_other_till");
  const listApproval = (): ApproverRequest | undefined =>
    listAsks() ? { approver_code: listCode(), approver_pin: listPin() } : undefined;
  const listReady = () =>
    !listBusy() && (!listAsks() || (listCode().trim() !== "" && listPin().trim() !== ""));

  // One drawer picked to start fills in its own float, which the person may change; several start
  // each on its own.
  const listFloatText = () => {
    const [only] = pickedDrawers();
    return listFloatTyped() || only === undefined
      ? listAmount()
      : wholeUnits(only.default_float.amount_minor);
  };
  const typeListFloat = (text: string) => {
    setListFloatTyped(true);
    setListAmount(text);
  };
  const startReady = () =>
    listReady() && (pickedDrawers().length > 1 || parseAmount(listFloatText()) !== null);

  // A figure a close answered, or nothing where it answered none.
  const amountOf = (amount: Money | undefined) =>
    formatAmount(amount ?? money(storeCurrency(), 0));
  const drawerState = (drawer: DrawerEntry) => {
    switch (drawerPhase(drawer)) {
      case "closed":
        return t("shift.drawer_closed");
      case "counted":
        return t("shift.drawer_counted");
      default:
        return drawer.opened_clock === undefined
          ? t("shift.drawer_open")
          : t("shift.drawer_open_since", { time: drawer.opened_clock });
    }
  };

  // A tap picks a drawer, or puts it back. Drawers in one state go together, since they start or
  // close together; one in another state, or an open one, which is counted alone, picks afresh.
  const pickDrawer = (drawer: DrawerEntry) => {
    const key = tillKey(drawer);
    const current = pickedDrawers();
    setListError(null);
    setStartedDrawers(null);
    setCountedDrawer(null);
    setClosedDrawers(null);
    setListAmount("");
    setListFloatTyped(false);
    if (owedDrawers().length > 0) {
      setOwedDrawers([]);
      setGivenReasons({});
      setListCode("");
      setListPin("");
    }
    if (current.some((chosen) => tillKey(chosen) === key)) {
      setPicked(current.filter((chosen) => tillKey(chosen) !== key).map(tillKey));
      return;
    }
    const [first] = current;
    const together =
      drawerPhase(drawer) !== "open" &&
      first !== undefined &&
      drawerPhase(first) === drawerPhase(drawer);
    setPicked(together ? [...current.map(tillKey), key] : [key]);
  };

  // Runs one act on the list, and drops the approver after it whatever came of it, but for a close
  // refused until its drawers have a reason, which the reason step sends again at once with it.
  const onList = async (act: () => Promise<void>) => {
    setListBusy(true);
    setListError(null);
    let owed: ShiftResponse[] | null = null;
    try {
      await act();
    } catch (caught) {
      owed = owedVariances(caught);
      if (owed === null) {
        setListError(errorMessage(caught, DRAWER_REASONS));
      } else {
        setOwedDrawers(owed);
      }
    } finally {
      if (owed === null) {
        setListCode("");
        setListPin("");
      }
      setListBusy(false);
    }
  };

  // Starts each drawer picked, one request each, so the edge records each start, the approver's
  // code and PIN going with each other till's. The first refusal stops it — a wrong PIN is not tried
  // again against the lockout — and the screen says which started and which did not.
  const startDrawers = () =>
    onList(async () => {
      const chosen = pickedDrawers();
      const approval = listApproval();
      const typed = chosen.length === 1 ? parseAmount(listFloatText()) : null;
      const started: string[] = [];
      let missed: DrawerEntry[] = [];
      let refusal: string | null = null;
      for (const [index, drawer] of chosen.entries()) {
        const till = tillKey(drawer);
        try {
          await openDrawerShift(
            till,
            typed ?? drawer.default_float.amount_minor,
            till === ownTill() ? undefined : approval,
          );
          started.push(drawerName(drawer));
        } catch (caught) {
          refusal = errorMessage(caught, DRAWER_REASONS);
          missed = chosen.slice(index);
          break;
        }
      }
      setPicked(missed.map(tillKey));
      setListAmount("");
      setListFloatTyped(false);
      setStartedDrawers({ started, missed: missed.map(drawerName), refusal });
    });

  // The blind count of the one open drawer picked: what it should hold is on screen only where the
  // read carries it, never worked out here.
  const countDrawer = () =>
    onList(async () => {
      const [drawer] = pickedDrawers();
      const value = parseAmount(listAmount());
      if (drawer === undefined || drawer.shift === null || value === null) {
        return;
      }
      await countDrawerShift(drawer.shift.shift_id, value, listApproval());
      setCountedDrawer(drawerName(drawer));
      setPicked([]);
      setListAmount("");
    });

  // Closes the counted drawers picked: one on its own, or several in one act, every one or none,
  // under one approver (decision 10). Each drawer's figures come back, with what printed.
  const closeDrawers = () =>
    onList(async () => {
      const names = new Map<string, string>();
      for (const drawer of pickedDrawers()) {
        if (drawer.shift !== null) {
          names.set(drawer.shift.shift_id, drawerName(drawer));
        }
      }
      const closed = await closeDrawerShifts([...names.keys()], listApproval(), givenReasons());
      setOwedDrawers([]);
      setGivenReasons({});
      setClosedDrawers({
        drawers: closed.shifts.map((closedShift) => ({
          name: names.get(closedShift.shift_id) ?? "",
          shift: closedShift,
        })),
        slip: closed.slip,
      });
      setPicked([]);
    });

  // A reason for one drawer owed one; once each has its reason, the close goes again with them.
  const drawerVarianceReason = (shiftId: string, reasonCodeId: string) => {
    const given = { ...givenReasons(), [shiftId]: reasonCodeId };
    setGivenReasons(given);
    if (owedDrawers().every((owed) => given[owed.shift_id] !== undefined)) {
      void closeDrawers();
    }
  };
  const owedName = (owed: ShiftResponse) => {
    const listed = drawers().find((drawer) => drawer.shift?.shift_id === owed.shift_id);
    return listed === undefined ? "" : drawerName(listed);
  };

  return (
    <section
      class="mx-auto max-w-md p-4"
      classList={{
        "tablet:grid tablet:max-w-5xl tablet:grid-cols-2 tablet:items-start tablet:gap-6":
          listShown(),
      }}
    >
      {/* This device's own drawer, and beside it on a tablet every till's (ADR-0167). */}
      <div>
      <PageHeader title={t("shift.title")} />

      <Show when={error()}>
        {(message) => (
          <p class="mb-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
            {message()}
          </p>
        )}
      </Show>

      {/* Worded for each way, since a store may change either way and waits all the same. */}
      <Show when={waiting()}>
        {(model) => (
          <p
            class="mb-3 rounded-token border border-awaiting px-3 py-2 text-ink"
            role="status"
            data-outcome="model-waiting"
          >
            {t(
              model() === "DRAWER_MODEL_PER_TERMINAL"
                ? "shift.waiting_per_till"
                : "shift.waiting_per_store",
            )}
          </p>
        )}
      </Show>

      <Show when={noTill()}>
        <p
          class="mb-3 rounded-token border border-awaiting px-3 py-2 text-ink"
          role="status"
          data-outcome="no-till"
        >
          {t("shift.not_a_till")}
        </p>
      </Show>

      <Show when={ownDrawer()?.name}>
        {(name) => (
          <h2 class="mb-3 font-semibold" data-outcome="own-drawer">
            {t("shift.own_drawer", { name: name() })}
          </h2>
        )}
      </Show>

      {/* Still open once the business day it opened on has ended (decision 11). */}
      <Show when={shift()?.dayEnded === true && phase() !== "SHIFT_STATE_CLOSED"}>
        <p
          class="mb-3 rounded-token border border-awaiting px-3 py-2 text-ink"
          role="status"
          data-outcome="day-ended"
        >
          {t("shift.day_ended")}
        </p>
      </Show>

      <Show
        when={
          (phase() === "NONE" || phase() === "SHIFT_STATE_CLOSED") &&
          can("cash.shift.open") &&
          !noTill()
        }
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
              void closeOwn(() => closeShift(current.shiftId));
            }
          }}
        >
          {t("shift.close")}
        </button>
        </Show>
        <Show when={owedHere()}>
          {(owed) => (
            <div class="mt-3 rounded-token border border-line bg-surface p-3" data-outcome="variance-owed">
              <p class="font-semibold">{t("shift.variance_reason")}</p>
              <p class="mt-1 text-sm text-ink-muted tabular-nums">
                {t("shift.drawer_figures", {
                  expected: amountOf(owed().expected_amount),
                  counted: amountOf(owed().counted_amount),
                })}
              </p>
              <p class="text-danger tabular-nums">
                {t("shift.variance")} {amountOf(owed().variance)}
              </p>
              <div class="mt-2 grid grid-cols-2 gap-2">
                <For each={reasonsFor(CASH_VARIANCE)}>
                  {(reason) => (
                    <button
                      type="button"
                      class="min-h-touch rounded-token border border-line bg-surface px-3 text-left"
                      data-step="varianceReason"
                      onClick={() => varianceReason(reason.reason_code_id)}
                    >
                      {reason.display_name}
                    </button>
                  )}
                </For>
              </div>
              <button
                type="button"
                class="mt-3 min-h-touch rounded-token border border-line px-3 text-sm"
                onClick={() => setOwedClose(null)}
              >
                {t("common.cancel")}
              </button>
            </div>
          )}
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
            <Show when={can("cash.drawer.open_no_sale") && !noTill()}>
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
      </div>

      <Show when={listShown()}>
        <div class="mt-6 tablet:mt-0" data-outcome="drawers">
          <h2 class="font-semibold">{t("shift.drawers_title")}</h2>
          <p class="mt-1 text-sm text-ink-muted">{t("shift.drawers_hint")}</p>
          <div class="mt-2 grid grid-cols-1 gap-2">
            <For each={drawers()}>
              {(drawer) => (
                <button
                  type="button"
                  class="min-h-touch rounded-token border border-line bg-surface p-3 text-left"
                  classList={{ "border-primary": picked().includes(tillKey(drawer)) }}
                  aria-pressed={picked().includes(tillKey(drawer))}
                  data-step="pickDrawer"
                  onClick={() => pickDrawer(drawer)}
                >
                  <span class="flex items-baseline justify-between gap-2">
                    <span class="font-semibold">{drawerName(drawer)}</span>
                    <Show when={drawer.terminal_device_id === ownTill()}>
                      <span class="text-sm text-ink-muted">{t("shift.drawer_this_till")}</span>
                    </Show>
                  </span>
                  <span class="block text-sm text-ink-muted">{drawerState(drawer)}</span>
                  <span class="block text-sm text-ink-muted tabular-nums">
                    {t("shift.drawer_float", { amount: formatAmount(drawer.default_float) })}
                  </span>
                  {/* Only where the read carries it: never worked out here. */}
                  <Show when={drawer.shift?.expected_amount}>
                    {(expected) => (
                      <span class="block text-sm tabular-nums">
                        {t("shift.expected_now", { amount: formatAmount(expected()) })}
                      </span>
                    )}
                  </Show>
                  <Show when={drawer.shift?.day_ended === true}>
                    <span class="mt-1 block text-sm font-semibold text-danger" data-outcome="drawer-day-ended">
                      {t("shift.drawer_day_ended")}
                    </span>
                  </Show>
                </button>
              )}
            </For>
          </div>

          <Show when={pickedPhase()}>
            {(picking) => (
              <div class="mt-3 rounded-token border border-line bg-surface p-3">
                <Show when={picking() === "closed" && pickedDrawers().length === 1}>
                  <label class="block text-sm text-ink-muted" for="drawer-float">
                    {t("shift.float_label", { currency: storeCurrency() })}
                  </label>
                  <input
                    id="drawer-float"
                    inputmode="numeric"
                    class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
                    value={listFloatText()}
                    onInput={(event) => typeListFloat(event.currentTarget.value)}
                  />
                  <Keypad value={listFloatText()} onChange={typeListFloat} />
                </Show>
                <Show when={picking() === "closed" && pickedDrawers().length > 1}>
                  <p class="text-sm text-ink-muted">{t("shift.drawers_own_floats")}</p>
                </Show>
                <Show when={picking() === "open"}>
                  <label class="block text-sm text-ink-muted" for="drawer-count">
                    {t("shift.count_label", { currency: storeCurrency() })}
                  </label>
                  <input
                    id="drawer-count"
                    inputmode="numeric"
                    class="mt-1 w-full rounded-token border border-line bg-surface p-3 tabular-nums"
                    value={listAmount()}
                    onInput={(event) => setListAmount(event.currentTarget.value)}
                  />
                  <Keypad value={listAmount()} onChange={setListAmount} />
                </Show>
                <Show when={listAsks()}>
                  <p class="mt-3 text-sm text-ink-muted">{t("shift.other_till_manager")}</p>
                  <ApproverFields
                    id="drawer-approver"
                    code={listCode()}
                    pin={listPin()}
                    onCode={setListCode}
                    onPin={setListPin}
                    codeLabel={t("shift.approver_code")}
                    pinLabel={t("shift.approver_pin")}
                  />
                </Show>
                <Show when={picking() === "closed"}>
                  <button
                    type="button"
                    class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
                    disabled={!startReady()}
                    data-step="startDrawers"
                    onClick={() => void startDrawers()}
                  >
                    {t("shift.start_drawers", { count: pickedDrawers().length })}
                  </button>
                </Show>
                <Show when={picking() === "open"}>
                  <button
                    type="button"
                    class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
                    disabled={!listReady() || parseAmount(listAmount()) === null}
                    data-step="countDrawer"
                    onClick={() => void countDrawer()}
                  >
                    {t("shift.enter_count")}
                  </button>
                </Show>
                <Show when={picking() === "counted"}>
                  <button
                    type="button"
                    class="mt-3 min-h-touch w-full rounded-token bg-primary font-semibold text-primary-ink disabled:opacity-50"
                    disabled={!listReady()}
                    data-step="closeDrawers"
                    onClick={() => void closeDrawers()}
                  >
                    {t("shift.close_drawers", { count: pickedDrawers().length })}
                  </button>
                </Show>
                <Show when={picking() === "counted" && owedDrawers().length > 0}>
                  <div class="mt-3" data-outcome="drawers-variance-owed">
                    <p class="font-semibold">{t("shift.variance_reason")}</p>
                    <For each={owedDrawers()}>
                      {(owed) => (
                        <div class="mt-3 tabular-nums" data-outcome="drawer-variance-owed">
                          <p class="font-semibold">{owedName(owed)}</p>
                          <p class="text-sm text-ink-muted">
                            {t("shift.drawer_figures", {
                              expected: amountOf(owed.expected_amount),
                              counted: amountOf(owed.counted_amount),
                            })}
                          </p>
                          <p class="text-danger">
                            {t("shift.variance")} {amountOf(owed.variance)}
                          </p>
                          <div class="mt-2 grid grid-cols-2 gap-2">
                            <For each={reasonsFor(CASH_VARIANCE)}>
                              {(reason) => (
                                <button
                                  type="button"
                                  class="min-h-touch rounded-token border border-line bg-surface px-3 text-left disabled:opacity-50"
                                  classList={{
                                    "border-primary":
                                      givenReasons()[owed.shift_id] === reason.reason_code_id,
                                  }}
                                  aria-pressed={givenReasons()[owed.shift_id] === reason.reason_code_id}
                                  disabled={listBusy()}
                                  data-step="drawerVarianceReason"
                                  onClick={() =>
                                    drawerVarianceReason(owed.shift_id, reason.reason_code_id)
                                  }
                                >
                                  {reason.display_name}
                                </button>
                              )}
                            </For>
                          </div>
                        </div>
                      )}
                    </For>
                  </div>
                </Show>
              </div>
            )}
          </Show>

          <Show when={listError()}>
            {(message) => (
              <p class="mt-3 rounded-token border border-danger px-3 py-2 text-danger" role="alert">
                {message()}
              </p>
            )}
          </Show>
          <Show when={startedDrawers()}>
            {(done) => (
              <div class="mt-3 text-sm" role="status">
                <Show when={done().started.length > 0}>
                  <p data-outcome="drawers-started">
                    {t("shift.drawers_started", { names: done().started.join(", ") })}
                  </p>
                </Show>
                <Show when={done().missed.length > 0}>
                  <p class="text-danger" data-outcome="drawers-not-started">
                    {t("shift.drawers_not_started", { names: done().missed.join(", ") })}
                  </p>
                </Show>
                <Show when={done().refusal}>{(refusal) => <p class="text-danger">{refusal()}</p>}</Show>
              </div>
            )}
          </Show>
          <Show when={countedDrawer()}>
            {(name) => (
              <p class="mt-3 text-sm" role="status" data-outcome="drawer-counted">
                {t("shift.drawer_counted_done", { name: name() })}
              </p>
            )}
          </Show>
          <Show when={closedDrawers()}>
            {(done) => (
              <div class="mt-3 rounded-token border border-line bg-surface p-4" data-outcome="drawers-closed">
                <p class="font-semibold">{t("shift.drawers_closed", { count: done().drawers.length })}</p>
                <For each={done().drawers}>
                  {(closed) => (
                    <div class="mt-3 tabular-nums" data-outcome="drawer-closed">
                      <p class="font-semibold">{closed.name}</p>
                      <p class="text-ink-muted">
                        {t("shift.drawer_figures", {
                          expected: amountOf(closed.shift.expected_amount),
                          counted: amountOf(closed.shift.counted_amount),
                        })}
                      </p>
                      <p
                        classList={{
                          "text-danger": (closed.shift.variance?.amount_minor ?? 0) !== 0,
                          "text-ok": (closed.shift.variance?.amount_minor ?? 0) === 0,
                        }}
                      >
                        {t("shift.variance")} {amountOf(closed.shift.variance)}
                      </p>
                      <Show when={closed.shift.shift_report_print}>
                        {(outcome) => (
                          <p class="text-sm text-ink-muted" role="status">
                            {t(printOutcomeKey(outcome(), "shift.report_printed"))}
                          </p>
                        )}
                      </Show>
                    </div>
                  )}
                </For>
                {/* One slip for them all, where the store's close report combines them. */}
                <Show when={done().slip}>
                  {(slip) => (
                    <p class="mt-3 text-sm text-ink-muted" role="status" data-outcome="drawers-slip">
                      {t(printOutcomeKey(slip(), "shift.combined_printed"))}
                    </p>
                  )}
                </Show>
              </div>
            )}
          </Show>
        </div>
      </Show>
    </section>
  );
}
