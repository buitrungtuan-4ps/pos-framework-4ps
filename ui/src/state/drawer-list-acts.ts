// Every till's drawer as money acts (ADR-0167, ADR-0196, ADR-0204): starting one or several,
// counting one, and closing one or several together. Each is pending until the edge answers, its
// refusal is shown in place to dismiss, and, with no answer, it is checked by reading the store's
// drawers again, never sent again. A re-read shows what the edge holds: a drawer started, counted
// or closed, and for a close the figures only its answer gives are left to the shift's report.
//
// A close over or short by more than the store allows is the edge's to judge; its refusal comes
// back as the drawers owed a reason (`owed`), as it did before.

import { api } from "../api/client";
import type { DrawerEntry, ShiftResponse } from "../api/schemas";
import { t, type MessageKey } from "../i18n";
import { errorMessage, owedVariances } from "../lib/errors";
import { type MoneyPhase, type Recheck, useMoneyAct } from "./money-act";
import { closeDrawerShifts, countDrawerShift, loadDrawers, loadShift, openDrawerShift } from "./store";

/** What a committed act on the list came to. */
export type ListDone =
  | { readonly started: string }
  | { readonly counted: string }
  | { readonly closed: { shifts: ShiftResponse[]; slip?: string } }
  | { readonly closedRead: true }
  | { readonly owed: ShiftResponse[] };

async function listed(): Promise<readonly DrawerEntry[]> {
  return (await api.drawers()).drawers;
}

/** After a re-read found the act made: the list and this till's own shift as the edge has them. */
async function refresh(): Promise<void> {
  await loadDrawers();
  await loadShift();
}

/** A start with no answer: made once the till's drawer has an open shift. */
export async function recheckStarted(till: string, name: string): Promise<Recheck<ListDone>> {
  const drawer = (await listed()).find((each) => each.terminal_device_id === till);
  if (drawer?.shift?.state !== "SHIFT_STATE_OPEN") {
    return { refused: t("shift.open_not_made") };
  }
  await refresh();
  return { committed: { started: name } };
}

/** A count with no answer: made once that shift reads counted. */
export async function recheckCountedDrawer(shiftId: string, name: string): Promise<Recheck<ListDone>> {
  const shift = (await listed()).find((each) => each.shift?.shift_id === shiftId)?.shift;
  if (shift?.state === "SHIFT_STATE_COUNTED") {
    await refresh();
    return { committed: { counted: name } };
  }
  return { refused: t(shift === undefined || shift === null ? "shift.ended_unconfirmed" : "shift.count_not_made") };
}

/** A close with no answer: made once none of its shifts is still listed, every one or none. */
export async function recheckClosedDrawers(shiftIds: readonly string[]): Promise<Recheck<ListDone>> {
  const open = new Set((await listed()).flatMap((each) => (each.shift === null ? [] : [each.shift.shift_id])));
  if (shiftIds.some((id) => open.has(id))) {
    return { refused: t("shift.close_not_made") };
  }
  await refresh();
  return { committed: { closedRead: true } };
}

/** Sends a close; drawers owed a reason come back as their figures rather than as a refusal. */
async function sendClose(
  shiftIds: string[],
  approval: Parameters<typeof closeDrawerShifts>[1],
  reasons: Readonly<Record<string, string>>,
): Promise<ListDone> {
  try {
    return { closed: await closeDrawerShifts(shiftIds, approval, reasons) };
  } catch (caught) {
    const owed = owedVariances(caught);
    if (owed === null) throw caught;
    return { owed };
  }
}

/** The list's acts, one at a time, with `onDone` told what each committed one came to. */
export function useDrawerListAct(onDone: (done: ListDone) => void, reasons: Readonly<Record<string, MessageKey>>) {
  let recheck: (subject: string) => Promise<Recheck<ListDone>> = async () => null;
  let kind: "open" | "count" | "close" = "open";
  const act = useMoneyAct<ListDone>({
    act: "drawer_list",
    recheck: (subject) => recheck(subject),
    committed: onDone,
    refusalMessage: (caught) => errorMessage(caught, reasons),
  });
  const run = (subject: string, send: () => Promise<ListDone>, check: () => Promise<Recheck<ListDone>>) => {
    recheck = check;
    return act.run(subject, () => null, send);
  };
  return {
    ...act,
    /** Whether the act just run left nothing out: committed, with no refusal and no answer awaited. */
    went: () => act.phase() === "idle" && act.refusal() === null,
    pending: (): { act: "open" | "count" | "close"; phase: MoneyPhase } | null =>
      act.phase() === "idle" ? null : { act: kind, phase: act.phase() },
    /** Starts `till`'s drawer on `minor`. */
    start: (till: string, name: string, minor: number, approval?: Parameters<typeof openDrawerShift>[2], assignee?: string) => {
      kind = "open";
      const send = () => openDrawerShift(till, minor, approval, assignee).then((): ListDone => ({ started: name }));
      return run(`start:${till}`, send, () => recheckStarted(till, name));
    },
    /** Counts the one open drawer picked. */
    count: (shiftId: string, name: string, minor: number, approval?: Parameters<typeof countDrawerShift>[2]) => {
      kind = "count";
      const send = () => countDrawerShift(shiftId, minor, approval).then((): ListDone => ({ counted: name }));
      return run(shiftId, send, () => recheckCountedDrawer(shiftId, name));
    },
    /** Closes the counted drawers picked, with the reasons given where the store asked them. */
    close: (shiftIds: string[], approval?: Parameters<typeof closeDrawerShifts>[1], reasons: Readonly<Record<string, string>> = {}) => {
      kind = "close";
      return run([...shiftIds].sort().join(","), () => sendClose(shiftIds, approval, reasons), () =>
        recheckClosedDrawers(shiftIds),
      );
    },
  };
}
