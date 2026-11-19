// A kiosk's order (ADR-0224 decisions 4 and 5): a guest's taps ring the kiosk's own tableless
// order, opened on the first; the guest sends it to be paid at the counter, which opens its bill,
// and is shown the number it is called by. The kiosk signs in as its own staff record, which the
// edge holds to opening an order, ringing and removing its lines, and sending it to be paid.
//
// An order nobody touches for the store's `kiosk.abandon_seconds` is given up: its unsent lines are
// voided and the order closed citing `ABANDONED`, the framework's reason, where the store's list
// gives it. A list that does not is never answered with another reason: the order is then left open
// for staff, who see it among the counter's orders, and the kiosk starts over. An order sent to be
// paid is never touched.

import { createSignal } from "solid-js";

import { ApiError, api } from "../api/client";
import type { MenuResponse, ReasonCodesResponse } from "../api/schemas";

/** The framework's reason for an order a guest walked away from (`pos_proto::reason_codes`). */
const ABANDONED = "ABANDONED";
/** A guest's own change of mind, the reason a removed line cites where the store gives it. */
const CHANGED_MIND = "CUSTOMER_CHANGED_MIND";

export type KioskItem = MenuResponse["items"][number];
export interface KioskLine {
  readonly lineId: string;
  readonly item: KioskItem;
  readonly quantity: number;
}
export type KioskPhase = "start" | "ordering" | "done" | "closed";

const [phase, setPhase] = createSignal<KioskPhase>("start");
const [lines, setLines] = createSignal<readonly KioskLine[]>([]);
const [queueNumber, setQueueNumber] = createSignal<number | null>(null);
const [refusal, setRefusal] = createSignal<unknown>(null);
let orderId: string | null = null;
let reasons: ReasonCodesResponse["reasons"] = [];

export { lines, phase, queueNumber, refusal };

/** The store's reason list, read once the kiosk has signed in. */
export async function loadKioskReasons(): Promise<void> {
  reasons = (await api.reasonCodes()).reasons;
}

/** The id of the store's reason with `code` for `action`, if its list gives one. */
function reasonFor(code: string, action: string): string | undefined {
  return reasons.find((reason) => reason.code === code && reason.applies_to.includes(action))?.reason_code_id;
}

/** Whether the store's list asks no reason at all for `action`. */
function noReasonFor(action: string): boolean {
  return !reasons.some((reason) => reason.applies_to.includes(action));
}

/** Whether a guest may take a line back off: only where the store gives a reason for it. */
export function canRemove(): boolean {
  return reasonFor(CHANGED_MIND, "REASON_ACTION_VOID_LINE") !== undefined;
}

function quantity(count: number) {
  return { milli: count * 1000 };
}

/** `line` at its new count, as a new row so the cart draws it again. */
function recount(line: KioskLine, count: number): void {
  setLines(lines().map((kept) => (kept.lineId === line.lineId ? { ...kept, quantity: count } : kept)));
}

/** A fresh screen for the next guest. */
export function startOver(): void {
  orderId = null;
  setLines([]);
  setQueueNumber(null);
  setRefusal(null);
  setPhase("start");
}

async function run(act: () => Promise<void>): Promise<void> {
  setRefusal(null);
  try {
    await act();
  } catch (caught) {
    if (caught instanceof ApiError && caught.reason === "OPEN_SHIFT_REQUIRED") {
      setPhase("closed");
      return;
    }
    setRefusal(caught);
  }
}

/** Rings one of `item`, opening the kiosk's order on the first. */
export function ring(item: KioskItem): Promise<void> {
  return run(async () => {
    const same = lines().find((line) => line.item.menu_item_id === item.menu_item_id);
    if (same !== undefined) {
      await api.setLineQuantity(same.lineId, { quantity: quantity(same.quantity + 1) });
      recount(same, same.quantity + 1);
      return;
    }
    if (orderId === null) {
      const opened = await api.openOrder();
      orderId = opened.order_id;
      setQueueNumber(opened.queue_number);
    }
    const added = await api.addOrderLine(orderId, { menu_item_id: item.menu_item_id, quantity: quantity(1) });
    setLines([...lines(), { lineId: added.order_line_id, item, quantity: 1 }]);
    setPhase("ordering");
  });
}

/** One fewer of a line; the last one goes only where the store gives the guest's reason. */
export function fewer(line: KioskLine): Promise<void> {
  return run(async () => {
    if (line.quantity > 1) {
      await api.setLineQuantity(line.lineId, { quantity: quantity(line.quantity - 1) });
      recount(line, line.quantity - 1);
      return;
    }
    const reason = reasonFor(CHANGED_MIND, "REASON_ACTION_VOID_LINE");
    if (reason === undefined) return;
    await api.voidLine(line.lineId, { reason_code_id: reason });
    setLines(lines().filter((kept) => kept.lineId !== line.lineId));
  });
}

/** Sends the order to be paid at the counter: its bill opens, and the guest is shown its number. */
export function sendToPay(): Promise<void> {
  return run(async () => {
    if (orderId === null || lines().length === 0) return;
    await api.openBillForOrder(orderId);
    orderId = null;
    setPhase("done");
  });
}

/**
 * Gives up an order nobody touched (decision 5): voids its lines and closes it citing `ABANDONED`
 * where the store's list allows, and otherwise leaves it open for staff. Either way the kiosk starts
 * over. Called only while the guest is still ordering, so a bill has never been opened on it.
 */
export async function abandon(): Promise<void> {
  const order = orderId;
  if (phase() !== "ordering" || order === null) {
    startOver();
    return;
  }
  const voidReason = reasonFor(ABANDONED, "REASON_ACTION_VOID_LINE");
  const closeReason = reasonFor(ABANDONED, "REASON_ACTION_CLOSE_ORDER");
  const mayClose = closeReason !== undefined || noReasonFor("REASON_ACTION_CLOSE_ORDER");
  try {
    if (voidReason !== undefined && mayClose) {
      for (const line of lines()) {
        await api.voidLine(line.lineId, { reason_code_id: voidReason });
      }
      await api.closeOrder(order, closeReason);
    }
  } catch {
    // Whatever was not voided stays on the order, which staff see among the counter's orders.
  }
  startOver();
}
