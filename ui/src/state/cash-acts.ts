// Cash in and out of the drawer outside a sale as money acts (ADR-0165, ADR-0186, ADR-0196,
// ADR-0204): a paid in, a paid out and a drop to the safe, and the drawer opened without a sale.
//
// A movement is refused at once where the till can tell, pending until the edge answers, and, with
// no answer, checked by reading the drawer's shift again: made once the shift's total for that way
// has grown by the amount since a read taken just before the request. The shift then shows the
// edge's totals; what the drawer did is not known from a read, so it is not said.
//
// The drawer opened without a sale changes nothing a read can show. With no answer it is not rolled
// back and not sent again: the till says it is unconfirmed and the cashier, who can see the drawer,
// dismisses it (ADR-0204 decision 5).

import { createSignal } from "solid-js";

import { api } from "../api/client";
import type { ApproverRequest, CountBreakdown, DrawerOutcome, ShiftResponse } from "../api/schemas";
import { t } from "../i18n";
import { errorMessage, reasonMessage } from "../lib/errors";
import { PRECHECKED } from "../package/prechecks";
import { type MoneyPhase, type Recheck, isRefusal, useMoneyAct } from "./money-act";
import { recordSafeDrop } from "./safe-drop";
import { addsUp } from "./shift-acts";
import { applyShift, openDrawerNoSale, recordCashMovement, state } from "./store";

export type MovementKind = "paid_in" | "paid_out" | "safe_drop";

const ACTION = { paid_in: "cash.movement.paid_in", paid_out: "cash.movement.paid_out", safe_drop: "cash.movement.safe_drop" } as const;

/** The shift's running total a movement of `kind` adds to, as the edge reports it. */
function totalOf(shift: ShiftResponse, kind: MovementKind): number {
  const figure = { paid_in: shift.paid_in_amount, paid_out: shift.paid_out_amount, safe_drop: shift.dropped_amount }[kind];
  return figure?.amount_minor ?? 0;
}

/** What the till can tell about a movement before sending it: no open shift, or notes that do not add up. */
export function movementRefusal(kind: MovementKind, minor: number, breakdown?: CountBreakdown): string | null {
  const checked = PRECHECKED[ACTION[kind]] ?? [];
  if (checked.includes("SHIFT_NOT_OPEN") && state.shift?.state !== "SHIFT_STATE_OPEN") {
    return reasonMessage("SHIFT_NOT_OPEN");
  }
  return checked.includes("COUNT_DOES_NOT_ADD_UP") && !addsUp(minor, breakdown)
    ? reasonMessage("COUNT_DOES_NOT_ADD_UP")
    : null;
}

/** A movement as sent: which way, how much, and the shift's total for that way just before. */
interface Sent {
  readonly kind: MovementKind;
  readonly minor: number;
  before: number | null;
}

/** What a committed movement came to: the edge's answer, or a re-read with no word of the drawer. */
export interface Moved {
  readonly kind: MovementKind;
  readonly minor: number;
  readonly drawer: DrawerOutcome | null;
}

/** What the edge says now about a movement with no answer. */
export async function recheckMovement(shiftId: string, sent: Sent): Promise<Recheck<Moved>> {
  const current = await api.currentShift();
  if (current?.shift_id !== shiftId) {
    return { refused: t("shift.movement_ended") };
  }
  if (sent.before !== null && totalOf(current, sent.kind) - sent.before >= sent.minor) {
    applyShift(current);
    return { committed: { kind: sent.kind, minor: sent.minor, drawer: null } };
  }
  return { refused: t("shift.movement_not_made") };
}

/** The Shift screen's paid in, paid out and drop to the safe, one at a time. */
export function useMovementAct(onMoved: (moved: Moved) => void) {
  let sent: Sent = { kind: "paid_in", minor: 0, before: null };
  const act = useMoneyAct<Moved>({
    act: "movement",
    recheck: (shiftId) => recheckMovement(shiftId, sent),
    committed: onMoved,
  });
  const send = async (shiftId: string, request: () => Promise<DrawerOutcome | undefined>): Promise<Moved> => {
    const current = await api.currentShift();
    sent.before = current?.shift_id === shiftId ? totalOf(current, sent.kind) : null;
    return { kind: sent.kind, minor: sent.minor, drawer: (await request()) ?? null };
  };
  return {
    ...act,
    pending: (): { act: MovementKind; phase: MoneyPhase } | null =>
      act.phase() === "idle" ? null : { act: sent.kind, phase: act.phase() },
    /** A paid in or out of `minor`, for `reasonCodeId`. */
    move: (shiftId: string, way: "in" | "out", minor: number, reasonCodeId: string) => {
      sent = { kind: way === "in" ? "paid_in" : "paid_out", minor, before: null };
      void act.run(
        shiftId,
        () => movementRefusal(sent.kind, minor),
        () => send(shiftId, () => recordCashMovement(shiftId, way, minor, reasonCodeId)),
      );
    },
    /** A drop of `minor` to the safe, counted note by note where `breakdown` says so. */
    drop: (shiftId: string, minor: number, breakdown?: CountBreakdown, reasonCodeId?: string) => {
      sent = { kind: "safe_drop", minor, before: null };
      void act.run(
        shiftId,
        () => movementRefusal("safe_drop", minor, breakdown),
        () => send(shiftId, () => recordSafeDrop(shiftId, minor, breakdown, reasonCodeId)),
      );
    },
  };
}

/** The drawer opened without a sale: the edge's refusal in place, and no answer said unconfirmed. */
export function useNoSaleAct(onOpened: (outcome: DrawerOutcome) => void) {
  const [sending, setSending] = createSignal(false);
  const [refusal, setRefusal] = createSignal<string | null>(null);
  const open = async (reasonCodeId: string, approval: ApproverRequest | undefined) => {
    if (sending() || refusal() !== null) {
      return;
    }
    setSending(true);
    try {
      onOpened(await openDrawerNoSale(reasonCodeId, approval));
    } catch (caught) {
      setRefusal(isRefusal(caught) ? errorMessage(caught) : t("shift.no_sale_unconfirmed"));
    } finally {
      setSending(false);
    }
  };
  return { sending, refusal, dismiss: () => setRefusal(null), open };
}
