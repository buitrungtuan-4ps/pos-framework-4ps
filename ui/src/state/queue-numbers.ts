// The number a guest was given for a counter order, as the kitchen and pass boards show it
// (ADR-0146). Read from the counter list, which is a screen-owned read: outside the gated registry,
// retried with the engine's backoff (ADR-0173).

import { api } from "../api/client";
import { createRetry } from "./screen-read";
import { setQueueNumbers, state } from "./store";

// How long a board waits before asking the counter list again for a number it has not got. A
// counter order is numbered when it is opened, so one read almost always answers; the wait only
// stops an order that has no number from being asked about on every change to the board.
const QUEUE_NUMBER_READ_MS = 5_000;
let queueNumbersReadAt = 0;
// The board's latest set, and the one retry a failed read waits on (ADR-0173).
let queueNumbersWanted: readonly string[] = [];
const queueNumbersRetry = createRetry();

// The number the guest was given for a counter order, or undefined when the till has not read it,
// or the order was never given one.
export function queueNumberFor(orderId: string): number | undefined {
  return state.queueNumbers[orderId];
}

// Reads the counter list for the numbers of the counter orders on a board, keeping only those.
// A failed read keeps what the board had: it goes on showing the order's reference until a read
// answers. One the edge may recover from is tried again with the engine's backoff, for the board's
// latest set, so the numbers arrive without waiting for the board to change.
export async function refreshQueueNumbers(onBoard: readonly string[]): Promise<void> {
  queueNumbersWanted = onBoard;
  const now = Date.now();
  if (now - queueNumbersReadAt < QUEUE_NUMBER_READ_MS) {
    return;
  }
  queueNumbersReadAt = now;
  try {
    const counter = await api.openOrders();
    queueNumbersRetry.reset();
    const wanted = new Set(queueNumbersWanted);
    const next: Record<string, number> = {};
    for (const [orderId, number] of Object.entries(state.queueNumbers)) {
      if (wanted.has(orderId)) {
        next[orderId] = number;
      }
    }
    for (const order of counter) {
      if (order.queue_number !== undefined && wanted.has(order.order_id)) {
        next[order.order_id] = order.queue_number;
      }
    }
    setQueueNumbers(next);
  } catch (caught) {
    queueNumbersRetry.after(caught, () => {
      queueNumbersReadAt = 0;
      void refreshQueueNumbers(queueNumbersWanted);
    });
  }
}
