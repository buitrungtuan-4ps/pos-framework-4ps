// What the kitchen has not been told about yet, for the two screens that take money (pilot U1).
//
// A cashier could take payment for dishes nobody had sent: the order screen's Take payment and the
// counter's charge opened a bill whatever the lines' state, and settling does not fire anything. On
// a busy night a walk-in pays, the kitchen never gets the ticket, and no screen says so. So before
// either screen opens a bill, it asks whether to send the unsent lines first (`SendFirst`).
//
// Keyed like every other helper on the order screen: a table id, or a counter order's walk-in key.

import { createSignal } from "solid-js";

import { type OrderLine, fireOrder, holdWalkIn, linesForTable, walkInKey } from "./store";

/** The question a screen asks before it takes payment for an order with unsent lines. */
export interface SendFirstGate {
  /** The key being asked about, or `null` while nothing is asked. */
  readonly asking: () => string | null;
  /** How many lines on `key` the kitchen has not been sent. */
  readonly unsent: (key: string) => number;
  /**
   * Asks about `key` when it has unsent lines, and says whether it did — in which case payment
   * waits for the answer, and `pay` is what **Send and pay** goes on to once the lines are sent.
   * With nothing unsent it asks nothing and the caller takes payment as before.
   */
  readonly ask: (key: string, pay: () => void) => boolean;
  /** Sends the asked-about lines to the kitchen in one transaction, as Send does, then pays. */
  readonly sendAndPay: () => Promise<void>;
  readonly cancel: () => void;
}

export function createSendFirst(): SendFirstGate {
  const [asking, setAsking] = createSignal<string | null>(null);
  let then: () => void = () => undefined;
  // A line still sending counts: it is food the kitchen has not been sent either, and a counter
  // charged a moment after the tap would otherwise go ahead without asking.
  const unsent = (key: string) =>
    linesForTable(key).filter((line) => line.state === "ORDER_LINE_STATE_ADDED").length;
  return {
    asking,
    unsent,
    ask: (key, pay) => {
      const asks = unsent(key) > 0;
      then = pay;
      setAsking(asks ? key : null);
      return asks;
    },
    sendAndPay: async () => {
      const key = asking();
      if (key !== null) {
        await fireOrder(key);
        setAsking(null);
        then();
      }
    },
    cancel: () => setAsking(null),
  };
}

/**
 * The key a counter order's lines are read and fired under: its walk-in key, held so the order's
 * lines can be found by it whether the till started the order or the order arrived on the relay.
 */
export function counterKey(orderId: string): string {
  holdWalkIn(orderId);
  return walkInKey(orderId);
}

/** How many unsent lines wait on each course, counted in one pass over `lines`. */
export function waitingByCourse(lines: readonly OrderLine[]): Map<string, number> {
  const counts = new Map<string, number>();
  for (const line of lines) {
    if (line.courseId) {
      counts.set(line.courseId, (counts.get(line.courseId) ?? 0) + 1);
    }
  }
  return counts;
}
