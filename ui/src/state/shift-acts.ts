// Opening, counting and closing this till's own shift as money acts (ADR-0196, ADR-0204). Each is
// refused at once where the till can tell, pending until the edge answers, and, with no answer,
// checked by reading the drawer's shift again. A re-read shows the edge's figures, never the ones
// the till sent: a count another device entered differently is shown as the edge recorded it.
//
// A close over or short by more than the store allows is the edge's alone to judge (the till is not
// sent the limit, and a blind store not the expectation): its `VARIANCE_REASON_REQUIRED` is not a
// refusal to roll back but the figures to ask a reason with, so it comes back as `owed`.

import { createSignal } from "solid-js";

import { api } from "../api/client";
import type { CountBreakdown, ShiftResponse } from "../api/schemas";
import { t } from "../i18n";
import { owedVariances, reasonMessage } from "../lib/errors";
import { PRECHECKED } from "../package/prechecks";
import { type MoneyPhase, type Recheck, useMoneyAct } from "./money-act";
import { applyShift, closeShift, countShift, openShift, ownTill, type ShiftInfo, state } from "./store";

const OPEN = PRECHECKED["cash.shift.open"] ?? [];
const COUNT = PRECHECKED["cash.shift.count"] ?? [];

/** Whether a count's notes and loose amount come to its total, as the edge checks; no notes do. */
export function addsUp(minor: number, breakdown: CountBreakdown | undefined): boolean {
  const notes = breakdown?.denomination_counts;
  if (notes === undefined || notes.length === 0) {
    return true;
  }
  const sum = notes.reduce((total, note) => total + note.denomination.amount_minor * note.quantity, 0);
  return sum + (breakdown?.uncounted_amount?.amount_minor ?? 0) === minor;
}

/** What the till can tell about opening its drawer: a shift it holds still open, or a float that does not add up. */
export function openRefusal(held: ShiftInfo | null, minor: number, breakdown?: CountBreakdown): string | null {
  if (OPEN.includes("SHIFT_ALREADY_OPEN") && held !== null && held.state !== "SHIFT_STATE_CLOSED") {
    return reasonMessage("SHIFT_ALREADY_OPEN");
  }
  return OPEN.includes("COUNT_DOES_NOT_ADD_UP") && !addsUp(minor, breakdown)
    ? reasonMessage("COUNT_DOES_NOT_ADD_UP")
    : null;
}

/** What the till can tell about a count: notes that do not come to the total. */
export function countRefusal(minor: number, breakdown?: CountBreakdown): string | null {
  return COUNT.includes("COUNT_DOES_NOT_ADD_UP") && !addsUp(minor, breakdown)
    ? reasonMessage("COUNT_DOES_NOT_ADD_UP")
    : null;
}

/** An open with no answer: made once the drawer has an open shift, with the edge's float. */
export async function recheckOpen(): Promise<Recheck<ShiftResponse>> {
  const current = await api.currentShift();
  return current !== null && current.state === "SHIFT_STATE_OPEN"
    ? { committed: current }
    : { refused: t("shift.open_not_made") };
}

/** A count with no answer: made once the shift reads counted, with the count the edge recorded. */
export async function recheckCount(shiftId: string): Promise<Recheck<ShiftResponse>> {
  const current = await api.currentShift();
  if (current?.shift_id !== shiftId) {
    return { refused: t("shift.ended_unconfirmed") };
  }
  return current.state === "SHIFT_STATE_COUNTED" ? { committed: current } : { refused: t("shift.count_not_made") };
}

/**
 * A close with no answer: made once the drawer no longer has that shift open. A read before a close
 * never carries the variance, so the close's figures are those its event brought, or none.
 */
export async function recheckClose(shiftId: string): Promise<Recheck<ClosedOutcome>> {
  const current = await api.currentShift();
  return current?.shift_id === shiftId ? { refused: t("shift.close_not_made") } : { committed: { read: true } };
}

/** What a close came to: closed with its answer, closed as a re-read found it, or a reason owed. */
export type ClosedOutcome = { readonly read: true } | { readonly answered: true } | { readonly owed: ShiftResponse };

/**
 * The close: what it came to once committed. A reason owed brings its figures; a close a re-read
 * found, with no event to bring its figures yet, leaves none but the report's.
 */
function useCloseAct() {
  const [owed, setOwed] = createSignal<ShiftResponse | null>(null);
  const [unread, setUnread] = createSignal(false);
  const act = useMoneyAct<ClosedOutcome>({
    act: "close",
    recheck: recheckClose,
    committed: (outcome) => {
      const noFigures = "read" in outcome && state.shift?.state !== "SHIFT_STATE_CLOSED";
      setOwed("owed" in outcome ? outcome.owed : null);
      setUnread(noFigures);
      if (noFigures) applyShift(null);
    },
  });
  return { act, owed, unread, setOwed, setUnread };
}

/** This till's own shift acts: which is out, the refusal to dismiss, and what a close came to. */
export function useOwnShiftActs(onDone: () => void) {
  // An answer has already been applied by the store; a re-read's shift is applied here.
  const shiftAct = (act: string, recheck: (subject: string) => Promise<Recheck<ShiftResponse>>) =>
    useMoneyAct<ShiftResponse | null>({
      act,
      recheck,
      committed: (shift) => {
        if (shift !== null) applyShift(shift);
        onDone();
      },
    });
  const opening = shiftAct("open", recheckOpen);
  const counting = shiftAct("count", recheckCount);
  const close = useCloseAct();
  const closing = close.act;
  const acts = [
    ["open", opening],
    ["count", counting],
    ["close", closing],
  ] as const;
  return {
    owed: close.owed,
    unread: close.unread,
    clearOwed: () => close.setOwed(null),
    pending: (): { act: "open" | "count" | "close"; phase: MoneyPhase } | null => {
      const found = acts.find(([, act]) => act.phase() !== "idle");
      return found === undefined ? null : { act: found[0], phase: found[1].phase() };
    },
    refusal: () => opening.refusal() ?? counting.refusal() ?? closing.refusal(),
    dismiss: () => acts.forEach(([, act]) => act.dismiss()),
    resume: () => {
      const shiftId = state.shift?.shiftId;
      opening.resume(openSubject());
      if (shiftId !== undefined) {
        counting.resume(shiftId);
        closing.resume(shiftId);
      }
    },
    open: (minor: number, breakdown?: CountBreakdown) =>
      void opening.run(openSubject(), () => openRefusal(state.shift, minor, breakdown), () => openShift(minor, breakdown).then(() => null)),
    count: (shiftId: string, minor: number, breakdown?: CountBreakdown) =>
      void counting.run(shiftId, () => countRefusal(minor, breakdown), () => countShift(shiftId, minor, breakdown).then(() => null)),
    close: (shiftId: string, reasonCodeId?: string) => {
      close.setUnread(false);
      void closing.run(shiftId, () => null, () => closed(shiftId, reasonCodeId));
    },
  };
}

/** Which drawer an open is for: no shift id exists until the edge opens one. */
const openSubject = () => `open:${ownTill() ?? "store"}`;

/** Sends a close; a reason owed comes back as its figures rather than as a refusal. */
async function closed(shiftId: string, reasonCodeId: string | undefined): Promise<ClosedOutcome> {
  try {
    await closeShift(shiftId, reasonCodeId);
    return { answered: true };
  } catch (caught) {
    const [owed] = owedVariances(caught) ?? [];
    if (owed === undefined) {
      throw caught;
    }
    return { owed };
  }
}
