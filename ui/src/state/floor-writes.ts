// The floor's and the menu's edge writes, painted first (ADR-0196 decision 1, `./edge-write`):
// cleaning a table, giving a table seated by mistake back, opening a bill, and marking a dish sold
// out or back. Each shows at once; the edge's answer makes it so, a refusal puts it back, and no
// answer puts it back and reads its subject again (the floor, the open orders, the menu).

import { api } from "../api/client";
import type { BillResponse } from "../api/schemas";
import { editWrite } from "./edge-write";
import { tableReacts } from "./optimistic";
import { reload, rereadWhole } from "./engine";
import { loadLiveOrders, openBillFor, setSoldOut, state, updateStore } from "./store";

// Whole, with no tag: a table given back is free again, so the floor the edge then holds can be the
// very one this device read before the guests sat down, and a `304` would keep the undo in place.
const rereadFloor = () => {
  void rereadWhole("floor");
  void loadLiveOrders();
};

/** A table's state, shown as `next` at once; hands back how to put it back. */
function paintTable(tableId: string, next: string): () => void {
  const was = state.tableState[tableId];
  updateStore((draft) => {
    draft.tableState[tableId] = next;
  });
  return () =>
    updateStore((draft) => {
      if (was === undefined) {
        delete draft.tableState[tableId];
      } else {
        draft.tableState[tableId] = was;
      }
    });
}

/** A table that needed cleaning, free again. */
export async function clean(tableId: string): Promise<void> {
  await editWrite("sales.table.clean", {
    paint: () => paintTable(tableId, "TABLE_STATE_FREE"),
    send: () => api.cleanTable(tableId),
    confirmed: (response) =>
      updateStore((draft) => {
        draft.tableState[tableId] = response.state;
      }),
    reread: rereadFloor,
  });
}

// Gives a table seated by mistake straight back to the floor, while nothing is sold on it
// (ADR-0163). The edge ends its order owing nothing; the fan-out brings the same to every other
// device, and this one shows it at once.
export async function releaseTable(tableId: string): Promise<void> {
  const was = {
    state: state.tableState[tableId],
    seatedAt: state.seatedAt[tableId],
    order: state.tableOrder[tableId],
  };
  await editWrite("sales.table.release", {
    paint: () => {
      updateStore((draft) => {
        draft.tableState[tableId] = "TABLE_STATE_FREE";
        delete draft.seatedAt[tableId];
        if (was.order !== undefined) {
          draft.liveOrders[was.order] = false;
          delete draft.tableOrder[tableId];
          delete draft.orderTable[was.order];
        }
      });
      return () =>
        updateStore((draft) => {
          if (was.state !== undefined) draft.tableState[tableId] = was.state;
          if (was.seatedAt !== undefined) draft.seatedAt[tableId] = was.seatedAt;
          if (was.order !== undefined) {
            draft.liveOrders[was.order] = true;
            draft.tableOrder[tableId] = was.order;
            draft.orderTable[was.order] = tableId;
          }
        });
    },
    send: () => api.releaseTable(tableId),
    confirmed: (response) =>
      updateStore((draft) => {
        draft.tableState[tableId] = response.state;
      }),
    reread: rereadFloor,
  });
}

/** Opens the table's bill, or answers the one already open; the table waits for payment at once. */
export async function openBill(tableId: string): Promise<string> {
  const existing = openBillFor(tableId);
  if (existing !== undefined) {
    return existing;
  }
  const response = await editWrite("billing.bill.open", {
    paint: () => paintTable(tableId, "TABLE_STATE_AWAITING_PAYMENT"),
    send: () => api.openBill(tableId),
    confirmed: (opened) =>
      updateStore((draft) => {
        const open = draft.openBills[tableId] ?? [];
        if (!open.includes(opened.bill_id)) {
          draft.openBills[tableId] = [...open, opened.bill_id];
        }
        const order = draft.tableOrder[tableId];
        if (order !== undefined) {
          draft.billOrder[opened.bill_id] = order;
        }
        if (opened.table_state !== undefined) {
          draft.tableState[tableId] = opened.table_state;
        }
      }),
    reread: rereadFloor,
  });
  tableReacts("REACTION_TRIGGER_BILL_OPENED", tableId, { billId: response.bill_id });
  return response.bill_id;
}

/** Opens a counter order's bill; `reread` reads the counter's list again when no answer comes. */
export function openCounterBill(orderId: string, reread: () => void): Promise<BillResponse> {
  return editWrite("billing.bill.open", { send: () => api.openBillForOrder(orderId), reread });
}

// Staff mark an item sold out at this store (86), or bring it back. The fan-out brings the same fact
// to every other till; this device shows it at once.
export async function setItemSoldOut(menuItemId: string, soldOut: boolean): Promise<void> {
  await editWrite(soldOut ? "sales.item.mark_sold_out" : "sales.item.restore", {
    paint: () => {
      setSoldOut(menuItemId, soldOut);
      return () => setSoldOut(menuItemId, !soldOut);
    },
    send: () => (soldOut ? api.markSoldOut(menuItemId) : api.restoreItem(menuItemId)),
    confirmed: () => {
      if (!soldOut) {
        void reload("menu");
      }
    },
    // Whole: the edge's event may have landed before the undo, and a `304` would keep the undo.
    reread: () => void rereadWhole("menu"),
  });
}
