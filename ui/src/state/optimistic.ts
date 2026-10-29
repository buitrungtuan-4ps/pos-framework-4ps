// The three acts the till paints before the edge answers (ADR-0173 decision 4): adding a line (with
// its modifiers and note), changing a quantity, and moving a table. An allow-list: payment, voids,
// discounts, refunds, comps, fees, firing, splits and every drawer act wait for the edge, and adding
// to this list takes a new record.
//
// - **The screen moves at once.** The line, the quantity or the table changes on the tap, marked as
//   sending, and one request goes to the edge. Nothing is queued: the edge is the only writer, and a
//   write it did not answer fails now (decision 5).
// - **No amount is the device's.** A line that is sending shows no figure; the line's own amount
//   comes with the edge's event, and the bill's totals are always the edge's check. Payment waits,
//   and says why, while any write is in flight or any line is still sending.
// - **The edge decides.** Its event, or its answer, whichever lands first, confirms the change. A
//   refusal rolls it back and the screen shows the refusal's own sentence. A write with no answer
//   rolls back and the open orders are read again, because the edge may have made it.

import { createSignal } from "solid-js";

import { ApiError, api } from "../api/client";
import type { LineRequest, MenuItemResponse, OrderLineRequest } from "../api/schemas";
import { type Money, addMoney } from "../lib/money";
import { PENDING, type OrderLine, loadLiveOrders, moveOrder, state, updateStore, walkInOrder } from "./store";

// How long a write the edge answered waits for its event before the open orders are read again.
const EVENT_WAIT_MS = 1_500;

const [inFlight, setInFlight] = createSignal(0);
let provisionalCount = 0;

// Whether a write is on its way to the edge, or a line is still waiting for the edge's figures.
export function writesPending(): boolean {
  return inFlight() > 0 || Object.values(state.lines).some((line) => line.pending === true);
}

async function sending<T>(write: () => Promise<T>): Promise<T> {
  setInFlight((count) => count + 1);
  try {
    return await write();
  } finally {
    setInFlight((count) => count - 1);
  }
}

// The edge answered: if its event has not brought the line's own figures by now, read them.
function awaitEvent(lineId: string): void {
  setTimeout(() => {
    if (state.lines[lineId]?.pending === true) {
      void loadLiveOrders();
    }
  }, EVENT_WAIT_MS);
}

// A write the edge did not answer may still have been made: the open orders say whether it was.
function afterFailure(caught: unknown): never {
  if (!(caught instanceof ApiError)) {
    void loadLiveOrders();
  }
  throw caught;
}

// Adds one of the store's menu items to a table, or to a walk-in held under `tableId`. An item the
// price book says cannot be sold is refused before anything is painted.
export async function addItem(
  tableId: string,
  item: MenuItemResponse,
  modifiers: string[] = [],
  note?: string,
): Promise<void> {
  if (!item.available || item.tax_rate === undefined || item.tax_rate === null) {
    throw new Error(`${item.display_name} is not sellable`);
  }
  const rate = item.tax_rate;
  const walkIn = walkInOrder(tableId);
  // A walk-in's line names no price (the edge prices it at its own channel), so only a table's is
  // priced here.
  const unitPrice = walkIn === undefined ? withModifiers(item, modifiers) : item.unit_price;
  const ghost = `${PENDING}${(provisionalCount += 1)}`;
  const line: OrderLine = {
    orderLineId: ghost,
    orderId: state.tableOrder[tableId] ?? `${PENDING}order:${tableId}`,
    name: item.display_name,
    quantityMilli: 1000,
    // Never shown while the line is sending; the edge's event brings the line's own figure.
    lineTotal: unitPrice,
    state: "ORDER_LINE_STATE_ADDED",
    // Whose dish it is, where the store assigns seats and a server has said (never on a walk-in).
    seat: walkIn === undefined && state.seatsEnabled ? state.seatForTable[tableId] : undefined,
    // The course the catalog puts the item on, where the store runs courses (ADR-0130).
    courseId: state.coursesEnabled ? (item.course_id ?? undefined) : undefined,
    modifierMenuItemIds: modifiers,
    notePresent: note !== undefined,
    note,
    menuItemId: item.menu_item_id,
    pending: true,
  };
  updateStore((draft) => {
    draft.lines[ghost] = line;
  });
  try {
    const response = await sending(() =>
      walkIn === undefined
        ? api.addLine(tableId, tableLine(item, rate, unitPrice, line))
        : api.addOrderLine(walkIn, counterLine(item, line)),
    );
    updateStore((draft) => {
      delete draft.lines[ghost];
      draft.tableOrder[tableId] = response.order_id;
      if (walkIn === undefined) {
        draft.orderTable[response.order_id] = tableId;
      }
      // Unless the edge's event got here first, the line waits for it, still sending.
      draft.lines[response.order_line_id] ??= {
        ...line,
        orderLineId: response.order_line_id,
        orderId: response.order_id,
        state: response.state,
      };
    });
    awaitEvent(response.order_line_id);
  } catch (caught) {
    updateStore((draft) => {
      delete draft.lines[ghost];
    });
    afterFailure(caught);
  }
}

// One of the dish at the table's price book: its own price plus each chosen modifier's, which is
// how a large costs more than a small (ADR-0063). A table's line is priced here, by the device, and
// the edge records what it is sent, so a till that sent the dish's price alone charged the size
// shown on the picker to nobody (pilot run 2, N2). A modifier the price book does not hold cannot
// be priced, and is refused before anything is painted, as an unsellable dish is. A walk-in's line
// names no price: the edge prices it, modifiers and all (ADR-0146).
function withModifiers(item: MenuItemResponse, modifiers: string[]): Money {
  let unitPrice: Money = item.unit_price;
  for (const modifierId of modifiers) {
    const modifier = state.menu.find((candidate) => candidate.menu_item_id === modifierId);
    if (modifier === undefined) {
      throw new Error(`${item.display_name} cannot be priced with that choice`);
    }
    unitPrice = addMoney(unitPrice, modifier.unit_price);
  }
  return unitPrice;
}

// The request for a table's line: the price book's figures, with the modifiers' prices in them.
function tableLine(
  item: MenuItemResponse,
  rate: NonNullable<MenuItemResponse["tax_rate"]>,
  unitPrice: Money,
  line: OrderLine,
): LineRequest {
  return {
    menu_item_id: item.menu_item_id,
    display_name: item.display_name,
    quantity: { milli: 1000 },
    unit_price: unitPrice,
    line_total: unitPrice,
    tax_class_id: item.tax_class_id,
    tax_rate: rate,
    seat: line.seat,
    course_id: line.courseId,
    modifier_menu_item_ids: line.modifierMenuItemIds,
    note_present: line.notePresent === true,
    note: line.note,
  };
}

// The request for a walk-in's line, which names no price: the edge prices it at the order's own
// channel (ADR-0146).
function counterLine(item: MenuItemResponse, line: OrderLine): OrderLineRequest {
  return {
    menu_item_id: item.menu_item_id,
    quantity: { milli: 1000 },
    course_id: line.courseId,
    modifier_menu_item_ids: line.modifierMenuItemIds,
    note_present: line.notePresent === true,
    note: line.note,
  };
}

// Sets how many of a line, shown at once and sending until the edge's event brings its figure.
export async function setQuantity(lineId: string, quantityMilli: number): Promise<void> {
  const before = state.lines[lineId];
  if (before === undefined) {
    return;
  }
  const was = { quantityMilli: before.quantityMilli, pending: before.pending };
  updateStore((draft) => {
    const held = draft.lines[lineId];
    if (held !== undefined) {
      held.quantityMilli = quantityMilli;
      held.pending = true;
    }
  });
  try {
    const line = await sending(() =>
      api.setLineQuantity(lineId, { quantity: { milli: quantityMilli } }),
    );
    updateStore((draft) => {
      const held = draft.lines[line.order_line_id];
      if (held !== undefined) {
        held.state = line.state;
      }
    });
    awaitEvent(lineId);
  } catch (caught) {
    updateStore((draft) => {
      const held = draft.lines[lineId];
      if (held !== undefined) {
        held.quantityMilli = was.quantityMilli;
        held.pending = was.pending;
      }
    });
    afterFailure(caught);
  }
}

// Moves the guests and their order to another table, at once; a refusal moves them back.
export async function moveTable(from: string, to: string): Promise<void> {
  const order = state.tableOrder[from];
  const was = { from: state.tableState[from], to: state.tableState[to] };
  if (order !== undefined) {
    updateStore((draft) =>
      moveOrder(draft, order, from, to, "TABLE_STATE_NEEDS_CLEANING", "TABLE_STATE_OCCUPIED"),
    );
  }
  try {
    const response = await sending(() => api.transferTable(from, to));
    updateStore((draft) =>
      moveOrder(draft, response.order_id, from, to, response.from_table.state, response.to_table.state),
    );
  } catch (caught) {
    if (order !== undefined) {
      updateStore((draft) =>
        moveOrder(
          draft,
          order,
          to,
          from,
          was.to ?? "TABLE_STATE_FREE",
          was.from ?? "TABLE_STATE_OCCUPIED",
        ),
      );
    }
    afterFailure(caught);
  }
}
