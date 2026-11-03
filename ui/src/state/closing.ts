// What the store does when a bill or an order closes: the bill comes off its table, the table
// moves on, and an order off every screen is forgotten (`./store` folds the events that say so).

import type { StoreShape } from "./store";

// Takes a closed bill off its table's list: settled, voided, or split or merged away. Answers with
// the table and how many bills are still open on it, or null for a bill no table holds — a counter
// order's, or one this device never saw open.
export function closeBill(draft: StoreShape, billId: string): { table: string; stillOpen: number } | null {
  const table = Object.keys(draft.openBills).find((key) => draft.openBills[key]?.includes(billId));
  if (table === undefined) {
    return null;
  }
  const stillOpen = (draft.openBills[table] ?? []).filter((id) => id !== billId);
  if (stillOpen.length === 0) {
    delete draft.openBills[table];
  } else {
    draft.openBills[table] = stillOpen;
  }
  return { table, stillOpen: stillOpen.length };
}

// Whether any bill on this order is still open on any table.
function orderStillOwes(draft: StoreShape, orderId: string): boolean {
  return Object.values(draft.openBills).some((ids) =>
    ids.some((id) => draft.billOrder[id] === orderId),
  );
}

// Forgets an order's lines once it is off every screen: paid, closed owing nothing, or voided after
// a part was paid. Nothing draws them again, and keeping them made `lines` grow for as long as the
// till stayed open — every dish of the day — where every in-memory structure has a limit.
export function forgetLines(draft: StoreShape, orderId: string): void {
  for (const line of Object.values(draft.lines)) {
    if (line.orderId === orderId) {
      delete draft.lines[line.orderLineId];
      delete draft.bumped[line.orderLineId];
    }
  }
}

// A bill paid, here or on another till. The table waits for cleaning once its last part is paid,
// or for the other guests of a split.
export function settleBill(draft: StoreShape, bill: string): void {
  const closed = closeBill(draft, bill);
  if (closed !== null) {
    if (closed.stillOpen === 0) {
      draft.tableState[closed.table] = "TABLE_STATE_NEEDS_CLEANING";
      delete draft.paidPart[closed.table];
    } else {
      // One guest of a split table has paid. The table waits for the others.
      draft.paidPart[closed.table] = true;
    }
  }
  // Paid, so off the kitchen board: its unbumped lines no longer wait on a table that is being
  // cleaned, and the till forgets them. Not while another part of it still owes — those guests'
  // food is still coming. An order with no table is paid before it is cooked, so its food stays on
  // the board until a station bumps it (ADR-0161), and the next read of the live orders forgets it.
  const settledOrder = draft.billOrder[bill];
  if (settledOrder !== undefined && !orderStillOwes(draft, settledOrder)) {
    draft.liveOrders[settledOrder] = false;
    if (draft.orderTable[settledOrder] === undefined) {
      draft.kitchenOrders[settledOrder] = true;
    } else {
      forgetLines(draft, settledOrder);
    }
  }
}

// What a table becomes once the last bill open on it is voided. Occupied again — the money was never
// taken and the order is still there to be charged — unless a part of it was already paid, when that
// meal is finished: the table wants cleaning and the order leaves the kitchen board, as on the edge.
export function afterLastVoid(draft: StoreShape, table: string, orderId: string | undefined): void {
  if (draft.paidPart[table]) {
    draft.tableState[table] = "TABLE_STATE_NEEDS_CLEANING";
    if (orderId !== undefined) {
      draft.liveOrders[orderId] = false;
      forgetLines(draft, orderId);
    }
  } else {
    draft.tableState[table] = "TABLE_STATE_OCCUPIED";
  }
  delete draft.paidPart[table];
}
