// The handover flow's state and acts (ADR-0187 decisions 1 to 3), apart from its panel
// (`components/Handover.tsx`).
//
// 1. The outgoing cashier counts blind and closes, giving a reason where the variance asks one.
// 2. The incoming cashier signs in on the same till, counts what is in the drawer, and opens on that
//    count, naming the shift just closed (`handed_over_from_shift_id`).
// 3. A count that differs from the outgoing one is counted once more, then confirmed as it stands.
//
// The flow holds the till between the two steps: while it is under way the Shift screen offers no
// other opening, so the next shift on this drawer is the linked one and no sale falls between them.
//
// The outgoing count and close, and the incoming opening, are money acts (ADR-0196, ADR-0204): with
// no answer the till reads the drawer's shift again rather than sending them again, and goes on
// with the edge's figures: the outgoing count the edge recorded, and the opening it made.

import { createRoot, createSignal } from "solid-js";

import { api } from "../api/client";
import type { ShiftResponse } from "../api/schemas";
import { errorMessage, owedVariances } from "../lib/errors";
import { money, type Money } from "../lib/money";
import { t } from "../i18n";
import { EMPTY_COUNT, fieldAmount, type CounterValue } from "./cash-count";
import { type MoneyPhase, type Recheck, useMoneyAct } from "./money-act";
import { loadSession, signedInAs } from "./session";
import { applyShift, closeShift, countShift, loadDrawers, state, storeCurrency } from "./store";

export type HandoverStep = "out" | "reason" | "signin" | "in" | "done";

const [step, setStep] = createSignal<HandoverStep | null>(null);
const [text, setText] = createSignal("");
const [count, setCount] = createSignal<CounterValue>(EMPTY_COUNT);
// The shift that closed, and what it counted out.
const [outgoing, setOutgoing] = createSignal<{ shiftId: string; counted: Money } | null>(null);
const [recounted, setRecounted] = createSignal(false);
// What the till asks of a count that differs: count once more, then confirm it as it stands.
const [ask, setAsk] = createSignal<"recount" | "confirm" | null>(null);
const [difference, setDifference] = createSignal<Money | null>(null);
const [printed, setPrinted] = createSignal<string | null>(null);
const [signingIn, setSigningIn] = createSignal(false);
const [signInError, setSignInError] = createSignal<string | null>(null);

/** What the outgoing act came to: the shift closed on the count the edge holds, or a reason owed. */
type Out = { readonly closed: { shiftId: string; counted: Money } } | { readonly owed: true };

/** An outgoing count and close with no answer, read again from the drawer's shift. */
export async function recheckOut(shiftId: string): Promise<Recheck<Out>> {
  const current = await api.currentShift();
  if (current?.shift_id === shiftId) {
    // The shift as the edge holds it: counted, the next tap closes it without counting again.
    applyShift(current);
    return { refused: t(current.state === "SHIFT_STATE_OPEN" ? "shift.count_not_made" : "shift.close_not_made") };
  }
  // Closed: on the count the edge recorded, which the count's answer or the close's event brings.
  const counted = state.shift?.shiftId === shiftId ? state.shift.counted : undefined;
  return counted === undefined ? null : { committed: { closed: { shiftId, counted } } };
}

/** An incoming opening with no answer: made once the drawer has the shift that took this one over. */
export async function recheckIn(fromShiftId: string): Promise<Recheck<ShiftResponse>> {
  const current = await api.currentShift();
  return current?.state === "SHIFT_STATE_OPEN" && current.handed_over_from_shift_id === fromShiftId
    ? { committed: current }
    : { refused: t("shift.open_not_made") };
}

// The figure the incoming count differed by when it was sent, said if the edge's answer has none.
let gapSent = 0;
const acts = createRoot(() => ({
  out: useMoneyAct<Out>({ act: "handover_out", recheck: recheckOut, committed: afterOut }),
  in: useMoneyAct<ShiftResponse>({ act: "handover_in", recheck: recheckIn, committed: afterIn }),
}));

/** Whether an act of the flow is with the edge, and which way it stands. */
export const busy = () => signingIn() || acts.out.phase() !== "idle" || acts.in.phase() !== "idle";
export const pending = (): { act: "close" | "open"; phase: MoneyPhase } | null =>
  acts.out.phase() !== "idle"
    ? { act: "close", phase: acts.out.phase() }
    : acts.in.phase() !== "idle"
      ? { act: "open", phase: acts.in.phase() }
      : null;
/** A refusal of the flow's money acts, to dismiss. */
export const refusal = () => acts.out.refusal() ?? acts.in.refusal();
export const dismiss = () => {
  acts.out.dismiss();
  acts.in.dismiss();
};

export { signInError as error, ask, count, difference, outgoing, printed, setCount, setText, step, text };

/** Whether a handover is under way on this till, between its first count and the incoming opening. */
export const handingOver = () => {
  const current = step();
  return current !== null && current !== "done";
};

/** What the count field records now, or `null` while it is not a figure. */
export const typed = () => fieldAmount(text(), count());

/** A drawer already counted closes on that count; an open one is counted in the flow first. */
export const countingOut = () => state.shift?.state === "SHIFT_STATE_OPEN";

function clearCount(): void {
  setText("");
  setCount(EMPTY_COUNT);
}


/** Starts a handover, or leaves one finished (`null`). */
export function goTo(next: HandoverStep | null): void {
  if (next === "out") {
    clearCount();
    setOutgoing(null);
    setRecounted(false);
    setAsk(null);
    setDifference(null);
    setPrinted(null);
    dismiss();
    setSignInError(null);
  }
  setStep(next);
}

function afterOut(done: Out): void {
  if ("owed" in done) {
    setStep("reason");
    return;
  }
  setOutgoing(done.closed);
  clearCount();
  setStep("signin");
}

/** Closes the outgoing shift; where the store asks a reason for its over or short, asks for one. */
async function sendClose(shiftId: string, reasonCodeId?: string): Promise<Out> {
  try {
    const closed = await closeShift(shiftId, reasonCodeId);
    return { closed: { shiftId: closed.shiftId, counted: closed.counted ?? money(storeCurrency(), 0) } };
  } catch (caught) {
    if (owedVariances(caught) === null) {
      throw caught;
    }
    return { owed: true };
  }
}

/** The close with the reason the store asked for. */
export const closeOut = (reasonCodeId?: string) => {
  const shift = state.shift;
  if (shift !== null) {
    void acts.out.run(shift.shiftId, () => null, () => sendClose(shift.shiftId, reasonCodeId));
  }
};

/** The outgoing count, where the drawer is still open, then the close. */
export const handOut = () => {
  const shift = state.shift;
  const value = typed();
  if (shift === null || (countingOut() && value === null)) {
    return;
  }
  void acts.out.run(shift.shiftId, () => null, async () => {
    if (countingOut() && value !== null) {
      await countShift(shift.shiftId, value.minor, value.breakdown);
    }
    return sendClose(shift.shiftId);
  });
};

/** The incoming cashier signs in on this till. */
export const signInIncoming = async (code: string, pin: string) => {
  setSigningIn(true);
  setSignInError(null);
  try {
    const result = await api.signIn(code, pin);
    if (!result.ok) {
      setSignInError(t(result.outcome === "locked_out" ? "signin.locked" : "signin.wrong"));
      return;
    }
    signedInAs(code, result.employeeId);
    await loadSession();
    setStep("in");
  } catch (caught) {
    setSignInError(errorMessage(caught));
  } finally {
    setSigningIn(false);
  }
};

function afterIn(opened: ShiftResponse): void {
  applyShift(opened);
  // The drawer list names the shift this one took over, which offers its slip again.
  void loadDrawers();
  setDifference(opened.handover_difference ?? money(storeCurrency(), gapSent));
  setPrinted(opened.handover_slip_print ?? null);
  setStep("done");
}

/**
 * The incoming count against the outgoing one: equal opens at once; different asks one recount, then
 * is opened on only once `confirmed`.
 */
export const handIn = (confirmed: boolean) => {
  const value = typed();
  const out = outgoing();
  if (value === null || out === null) {
    return;
  }
  const gap = value.minor - out.counted.amount_minor;
  if (gap !== 0 && !confirmed) {
    setDifference(money(storeCurrency(), gap));
    setAsk(recounted() ? "confirm" : "recount");
    if (!recounted()) {
      setRecounted(true);
      clearCount();
    }
    return;
  }
  setAsk(null);
  gapSent = gap;
  const opening = { currency_code: storeCurrency(), amount_minor: value.minor };
  void acts.in.run(out.shiftId, () => null, () =>
    api.openShift({ opening_float: opening, ...value.breakdown, handed_over_from_shift_id: out.shiftId }),
  );
};
