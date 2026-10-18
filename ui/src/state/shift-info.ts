// The till's shift as the edge answered it, and as a live event about it leaves it.
//
// The edge's answer to opening, counting or reading a shift carries its figures: the expectation
// where the store's count is not blind (ADR-0160), and what was paid in and out (ADR-0165). Its
// `cash.shift.opened` and `cash.shift.counted` events carry none of them, and the device that acted
// can hear its own event after its answer, so an event about the shift already held moves only its
// state, and its count where it carries one.

import type { ShiftResponse } from "../api/types";
import type { Money } from "../lib/money";
import type { ShiftInfo } from "./store";

// A shift as the edge answered it, every field named.
export function shiftInfo(response: ShiftResponse): ShiftInfo {
  return {
    shiftId: response.shift_id,
    state: response.state,
    expected: response.expected_amount,
    counted: response.counted_amount,
    variance: response.variance,
    reportPrint: response.shift_report_print,
    paidIn: response.paid_in_amount,
    paidOut: response.paid_out_amount,
    dayEnded: response.day_ended === true,
    assignee: response.assigned_employee_id,
  };
}

// The shift held, moved to the state an event about it names and keeping every figure it holds,
// or `null` when the event is about another shift, which the caller replaces and reads back.
export function keptShift(
  held: ShiftInfo | null,
  shiftId: string,
  shiftState: string,
  counted?: Money,
): ShiftInfo | null {
  if (held?.shiftId !== shiftId) {
    return null;
  }
  return { ...held, state: shiftState, counted: counted ?? held.counted };
}
