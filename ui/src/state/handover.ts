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

import { createSignal } from "solid-js";

import { api } from "../api/client";
import { errorMessage, owedVariances } from "../lib/errors";
import { money, type Money } from "../lib/money";
import { t } from "../i18n";
import { EMPTY_COUNT, fieldAmount, type CounterValue } from "./cash-count";
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
const [busy, setBusy] = createSignal(false);
const [error, setError] = createSignal<string | null>(null);

export { ask, busy, count, difference, error, outgoing, printed, setCount, setText, step, text };

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

async function act(action: () => Promise<void>): Promise<void> {
  setBusy(true);
  setError(null);
  try {
    await action();
  } catch (caught) {
    setError(errorMessage(caught));
  } finally {
    setBusy(false);
  }
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
    setError(null);
  }
  setStep(next);
}

/** Closes the outgoing shift; where the store asks a reason for its over or short, asks for one. */
export const closeOut = (reasonCodeId?: string) =>
  act(async () => {
    const shift = state.shift;
    if (shift === null) {
      return;
    }
    try {
      const closed = await closeShift(shift.shiftId, reasonCodeId);
      setOutgoing({ shiftId: closed.shiftId, counted: closed.counted ?? money(storeCurrency(), 0) });
      clearCount();
      setStep("signin");
    } catch (caught) {
      if (owedVariances(caught) === null) {
        throw caught;
      }
      setStep("reason");
    }
  });

/** The outgoing count, where the drawer is still open, then the close. */
export const handOut = () =>
  act(async () => {
    const shift = state.shift;
    const value = typed();
    if (shift === null || (countingOut() && value === null)) {
      return;
    }
    if (countingOut() && value !== null) {
      await countShift(shift.shiftId, value.minor, value.breakdown);
    }
    await closeOut();
  });

/** The incoming cashier signs in on this till. */
export const signInIncoming = (code: string, pin: string) =>
  act(async () => {
    const result = await api.signIn(code, pin);
    if (!result.ok) {
      setError(t(result.outcome === "locked_out" ? "signin.locked" : "signin.wrong"));
      return;
    }
    signedInAs(code, result.employeeId);
    await loadSession();
    setStep("in");
  });

/**
 * The incoming count against the outgoing one: equal opens at once; different asks one recount, then
 * is opened on only once `confirmed`.
 */
export const handIn = (confirmed: boolean) =>
  act(async () => {
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
    const opening = { currency_code: storeCurrency(), amount_minor: value.minor };
    const opened = await api.openShift({
      opening_float: opening,
      ...value.breakdown,
      handed_over_from_shift_id: out.shiftId,
    });
    applyShift(opened);
    // The drawer list names the shift this one took over, which offers its slip again.
    void loadDrawers();
    setDifference(opened.handover_difference ?? money(storeCurrency(), gap));
    setPrinted(opened.handover_slip_print ?? null);
    setStep("done");
  });
