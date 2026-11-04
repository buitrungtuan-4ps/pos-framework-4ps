// Sending lines to the kitchen and voiding a line, as edge writes that paint first (ADR-0196
// decision 1, `./edge-write`): the lines show sent, or voided, at once and marked as sending; the
// edge's answer makes it so, a refusal puts them back as they were, and no answer puts them back and
// reads the open orders again.

import { api } from "../api/client";
import type { ApproverRequest, LineResponse } from "../api/schemas";
import { editWrite } from "./edge-write";
import { loadLiveOrders, state, updateStore } from "./store";

type LineState = { readonly state: string; readonly pending: boolean | undefined };

/** The lines an act painted, and how each stood before it. */
type Painted = Map<string, LineState>;

/** Shows `lineIds` as `next`, sending, into `was`, and hands back how to put each back as it was. */
function paintLines(was: Painted, lineIds: readonly string[], next: string): () => void {
  was.clear();
  updateStore((draft) => {
    for (const id of lineIds) {
      const held = draft.lines[id];
      if (held !== undefined) {
        was.set(id, { state: held.state, pending: held.pending });
        held.state = next;
        held.pending = true;
      }
    }
  });
  return () =>
    updateStore((draft) => {
      for (const [id, before] of was) {
        const held = draft.lines[id];
        if (held !== undefined) {
          held.state = before.state;
          held.pending = before.pending;
        }
      }
    });
}

/** The edge's answer for each line, each painted line sending only if it was before. */
function answerLines(was: Painted, answered: readonly LineResponse[]): void {
  updateStore((draft) => {
    for (const [id, before] of was) {
      const held = draft.lines[id];
      if (held !== undefined) {
        held.pending = before.pending;
      }
    }
    for (const line of answered) {
      const held = draft.lines[line.order_line_id];
      if (held !== undefined) {
        held.state = line.state;
      }
    }
  });
}

/** The lines of `orderId` the kitchen has not been sent, on one course where `courseId` says so. */
function unsentOf(orderId: string, courseId?: string): string[] {
  return Object.values(state.lines)
    .filter(
      (line) =>
        line.orderId === orderId &&
        line.state === "ORDER_LINE_STATE_ADDED" &&
        line.pending !== true &&
        (courseId === undefined || line.courseId === courseId),
    )
    .map((line) => line.orderLineId);
}

const rereadOrders = () => void loadLiveOrders();

// Sends every unsent line on this table's order in one act — the operator's own unit of work.
//
// One tap and one request, where the screen used to put a Send button on every row: a table of six
// was six taps and six round trips, and a failure halfway left three lines with the kitchen and
// three not, with nothing on screen saying which. The edge commits them together or not at all.
//
// A table with no order, or nothing unsent, is not an error here any more than it is on the edge:
// there is nothing to send, and the button that calls this is disabled anyway.
export async function fireOrder(tableId: string): Promise<void> {
  const orderId = state.tableOrder[tableId];
  if (orderId === undefined) {
    return;
  }
  const was: Painted = new Map();
  await editWrite("sales.order.fire", {
    paint: () => paintLines(was, unsentOf(orderId), "ORDER_LINE_STATE_FIRED"),
    send: () => api.fireOrder(orderId, { station_id: state.defaultStation }),
    confirmed: (fired) => answerLines(was, fired),
    reread: rereadOrders,
  });
}

// Sends the unsent lines on one course of this table's order — "starters away" (ADR-0130).
//
// `fireOrder` narrowed to one course, and deliberately the same shape: one tap, one request, and the
// edge commits the course's lines together or not at all. The screen offers this only for courses
// that still have something waiting, so the empty case is not a button an operator can press — but
// it is answered rather than refused if a colleague gets there first, which is why nothing here
// treats an empty response as a failure.
export async function fireCourse(tableId: string, courseId: string): Promise<void> {
  const orderId = state.tableOrder[tableId];
  if (orderId === undefined) {
    return;
  }
  const was: Painted = new Map();
  await editWrite("sales.course.fire", {
    paint: () => paintLines(was, unsentOf(orderId, courseId), "ORDER_LINE_STATE_FIRED"),
    send: () => api.fireCourse(orderId, courseId, { station_id: state.defaultStation }),
    confirmed: (fired) => answerLines(was, fired),
    reread: rereadOrders,
  });
}

// Voids a line, citing a reason from the store's managed list (ADR-0115, §11 item 2).
//
// `approval` is the manager's badge and PIN, present only when the line has already fired: the
// kitchen has made it, stock has moved, and §5 puts that behind `VoidFiredLine` and a verified PIN.
// An unfired line is an ordinary cancel and the field is left off. The edge decides either way — it
// refuses without the approval when one is needed — so this passes what it was given rather than
// second-guessing the rule.
//
// The PIN is never held: it arrives as an argument, goes out on the one request, and the screen
// clears the field it was typed into.
export async function voidLine(lineId: string, reasonCodeId: string, approval?: ApproverRequest): Promise<void> {
  const was: Painted = new Map();
  await editWrite("sales.line.void", {
    paint: () => paintLines(was, [lineId], "ORDER_LINE_STATE_VOIDED"),
    send: () => api.voidLine(lineId, { reason_code_id: reasonCodeId, ...approval }),
    confirmed: (voided) => answerLines(was, [voided]),
    reread: rereadOrders,
  });
}
