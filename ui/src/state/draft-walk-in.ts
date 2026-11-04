// A walk-in that is not on the edge until its first item (UX-16).
//
// **New order** at the counter used to open an order on the edge at the tap. A guest who changed
// their mind before ordering left that order behind: it had a queue number and nothing on it, so the
// counter list — which shows what is owed — never listed it, and nothing could ever close it. The
// counter now opens the order screen on a draft, `/order/new`, and the order is opened on the edge
// by the first item added, on the channel the guest chose. Leaving before then leaves nothing.
//
// A first open the edge refuses opened nothing, so the next tap may try again. A first open with no
// answer may have opened an order, and the till cannot tell which open order is this draft's: the
// edge mints the id and its read names no opener. So the draft opens nothing more. Every later tap
// says the order is not confirmed, and the operator goes back and starts again (ADR-0204 decision
// 5). Never two orders from one draft.

import { createSignal } from "solid-js";

import { ApiError } from "../api/client";
import { t } from "../i18n";
import { Unconfirmed } from "../lib/errors";
import { DINE_IN, startWalkIn, walkInEatenIn, walkInKey, walkInSalesChannel } from "./store";

/** The order screen's id for a walk-in not opened yet. Never a real order's, which are ULIDs. */
export const DRAFT = "new";

const [channel, setChannel] = createSignal<string | undefined>(undefined);
let opening: Promise<string> | null = null;
let unconfirmed = false;

/** Starts a draft walk-in on `chosen` (the guest's answer, or none) and answers where it opens. */
export function draftWalkIn(chosen?: string): string {
  setChannel(chosen);
  opening = null;
  unconfirmed = false;
  return `/order/${DRAFT}`;
}

/** The channel a draft prices its menu at: the guest's answer, else the store's walk-in channel. */
export function draftChannel(): string {
  return channel() ?? walkInSalesChannel();
}

/** Whether a walk-in is eaten in, as `walkInEatenIn` answers, a draft's from the guest's answer. */
export function eatenIn(orderId: string): boolean | undefined {
  if (orderId !== DRAFT) {
    return walkInEatenIn(orderId);
  }
  const chosen = channel();
  return chosen === undefined ? undefined : chosen === DINE_IN;
}

/**
 * The key the order screen adds a line under. On a draft the order is opened first — once, however
 * many items are tapped while it opens — and the screen moves to the real order's address. Throws
 * the edge's refusal, after which a tap may open again, or `Unconfirmed` once an open went
 * unanswered, for that tap and every later one on the draft.
 */
export async function keyToAddTo(
  orderId: string,
  key: string,
  moveTo: (path: string) => void,
): Promise<string> {
  if (orderId !== DRAFT) {
    return key;
  }
  if (unconfirmed) {
    throw new Unconfirmed(t("order.draft_unconfirmed"));
  }
  const tried = (opening ??= startWalkIn(channel()));
  let opened: string;
  try {
    opened = await tried;
  } catch (caught) {
    if (caught instanceof ApiError) {
      // Refused: nothing opened, so the next tap may ask again.
      if (opening === tried) {
        opening = null;
      }
      throw caught;
    }
    unconfirmed = true;
    throw new Unconfirmed(t("order.draft_unconfirmed"));
  }
  moveTo(`/order/${opened}`);
  return walkInKey(opened);
}
