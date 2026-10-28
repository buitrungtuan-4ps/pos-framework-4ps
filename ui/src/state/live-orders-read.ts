// The live orders read, and the walk-ins it cannot know about yet (ADR-0160 decision 2).
//
// A read that began before a walk-in was opened cannot list it, and the store replaces its set of
// walk-in channels with each read's. So the till numbers each walk-in it opens with a guest's
// answer, a read records how many it had opened when it began, and applying the read keeps the
// answer the till holds for any opened since. An entry leaves at the first read that began after
// it, so the map holds only the opens between two reads.

import { api } from "../api/client";
import type { LiveOrder } from "../api/schemas";

let walkInsOpened = 0;
const walkInsUnread = new Map<string, number>();

// The live orders, and what the kitchen still has to make: the live orders again, plus each paid
// order with no table whose food no station has bumped (ADR-0161). An edge that predates the
// kitchen route answers 404, and the board then works from the live orders alone, as it always did.
export interface LiveOrdersRead {
  orders: LiveOrder[];
  kitchen: LiveOrder[] | null;
  // How many walk-ins this till had opened with a guest's answer when the read began.
  opensBefore: number;
}

export async function readLiveOrders(): Promise<LiveOrdersRead> {
  const opensBefore = walkInsOpened;
  const orders = await api.liveOrders();
  const kitchen = await api.kitchenOrders().catch(() => null);
  return { orders, kitchen, opensBefore };
}

// Records a walk-in this till opened with the guest's answer.
export function noteWalkInOpened(orderId: string): void {
  walkInsOpened += 1;
  walkInsUnread.set(orderId, walkInsOpened);
}

// The walk-ins opened after `read` began that it does not list: their channel is the till's own.
export function walkInsOpenedSince(read: LiveOrdersRead): string[] {
  const listed = new Set(read.orders.map((order) => order.order_id));
  const since: string[] = [];
  for (const [orderId, opened] of walkInsUnread) {
    if (opened <= read.opensBefore) {
      walkInsUnread.delete(orderId);
    } else if (!listed.has(orderId)) {
      since.push(orderId);
    }
  }
  return since;
}
