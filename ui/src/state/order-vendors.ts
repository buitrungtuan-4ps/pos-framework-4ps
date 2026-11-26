// The delivery vendor each open order came through, for its badge on the counter and the kitchen
// board (ADR-0248 decision 5). Learned from the reads that list orders, which name the vendor by the
// store's own name for it; an order no read has named a vendor for shows no badge.

import { createStore, reconcile } from "solid-js/store";
import type { LiveOrdersRead } from "./live-orders-read";

const [vendors, setVendors] = createStore<Record<string, string>>({});

// The store's name for the vendor `orderId` came through, or undefined for any other order.
export function orderVendor(orderId: string): string | undefined {
  return vendors[orderId];
}

// Records what a read of whole orders said. `replace` drops every order the read does not name,
// for a read that lists everything open; a partial read only adds.
export function noteOrderVendors(
  orders: readonly { order_id: string; vendor_name?: string }[],
  replace: boolean,
): void {
  const next: Record<string, string> = replace ? {} : { ...vendors };
  for (const order of orders) {
    if (order.vendor_name !== undefined) {
      next[order.order_id] = order.vendor_name;
    }
  }
  setVendors(reconcile(next));
}

// Records the vendors a live orders read names: the whole open set and the kitchen's, so it replaces.
export function noteLiveVendors(read: LiveOrdersRead): void {
  noteOrderVendors([...read.orders, ...(read.kitchen ?? [])], true);
}
